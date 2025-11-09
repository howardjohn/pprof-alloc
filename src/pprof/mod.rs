mod cast;
mod mappings;
#[path = "perftools.profiles.rs"]
mod proto;

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::write::GzEncoder;
use prost::Message;

pub use cast::CastFrom;
pub use cast::TryCastFrom;
pub use mappings::MAPPINGS;

/// Start times of the profiler.
#[derive(Copy, Clone, Debug)]
pub enum ProfStartTime {
	Instant(Instant),
	TimeImmemorial,
}

/// Helper struct to simplify building a `string_table` for the pprof format.
#[derive(Default)]
struct StringTable(BTreeMap<String, i64>);

impl StringTable {
	fn new() -> Self {
		// Element 0 must always be the emtpy string.
		let inner = [("".into(), 0)].into();
		Self(inner)
	}

	fn insert(&mut self, s: &str) -> i64 {
		if let Some(idx) = self.0.get(s) {
			*idx
		} else {
			let idx = i64::try_from(self.0.len()).expect("must fit");
			self.0.insert(s.into(), idx);
			idx
		}
	}

	fn finish(self) -> Vec<String> {
		let mut vec: Vec<_> = self.0.into_iter().collect();
		vec.sort_by_key(|(_, idx)| *idx);
		vec.into_iter().map(|(s, _)| s).collect()
	}
}

/// A single sample in the profile. The stack is a list of addresses.
#[derive(Clone, Debug)]
pub struct WeightedStack {
	pub addrs: Vec<u64>,
	pub weight: f64,
}

/// A mapping of a single shared object.
#[derive(Clone, Debug)]
pub struct Mapping {
	pub memory_start: usize,
	pub memory_end: usize,
	pub memory_offset: usize,
	pub file_offset: u64,
	pub pathname: PathBuf,
	pub build_id: Option<BuildId>,
}

/// Build ID of a shared object.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BuildId(pub Vec<u8>);

impl fmt::Display for BuildId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		for byte in &self.0 {
			write!(f, "{byte:02x}")?;
		}
		Ok(())
	}
}

/// A minimal representation of a profile that can be parsed from the jemalloc heap profile.
#[derive(Default)]
pub struct StackProfile {
	pub annotations: Vec<String>,
	// The second element is the index in `annotations`, if one exists.
	pub stacks: Vec<(WeightedStack, Option<usize>)>,
	pub mappings: Vec<Mapping>,
}

impl StackProfile {
	/// Converts the profile into the pprof format.
	///
	/// pprof encodes profiles as gzipped protobuf messages of the Profile message type
	/// (see `pprof/profile.proto`).
	pub fn to_pprof(
		&self,
		sample_type: (&str, &str),
		period_type: (&str, &str),
		anno_key: Option<String>,
	) -> Vec<u8> {
		let profile = self.to_pprof_proto(sample_type, period_type, anno_key);
		let encoded = profile.encode_to_vec();

		let mut gz = GzEncoder::new(Vec::new(), Compression::default());
		gz.write_all(&encoded).unwrap();
		gz.finish().unwrap()
	}

	/// Converts the profile into the pprof Protobuf format (see `pprof/profile.proto`).
	fn to_pprof_proto(
		&self,
		sample_type: (&str, &str),
		period_type: (&str, &str),
		anno_key: Option<String>,
	) -> proto::Profile {
		let mut profile = proto::Profile::default();
		profile.sample.reserve(self.stacks.len());
		let mut strings = StringTable::new();

		let anno_key = anno_key.unwrap_or_else(|| "annotation".into());

		profile.sample_type = vec![proto::ValueType {
			r#type: strings.insert(sample_type.0),
			unit: strings.insert(sample_type.1),
		}];
		profile.period_type = Some(proto::ValueType {
			r#type: strings.insert(period_type.0),
			unit: strings.insert(period_type.1),
		});

		profile.time_nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("now is later than UNIX epoch")
			.as_nanos()
			.try_into()
			.expect("the year 2554 is far away");

		for (mapping, mapping_id) in self.mappings.iter().zip(1..) {
			let pathname = mapping.pathname.to_string_lossy();
			let filename_idx = strings.insert(&pathname);

			let build_id_idx = match &mapping.build_id {
				Some(build_id) => strings.insert(&build_id.to_string()),
				None => 0,
			};

			profile.mapping.push(proto::Mapping {
				id: mapping_id,
				memory_start: 0,
				memory_limit: 0,
				file_offset: 0,
				filename: filename_idx,
				build_id: build_id_idx,
				..Default::default()
			});
		}

		let mut location_ids = BTreeMap::new();
		let mut function_ids = BTreeMap::new();
		for (stack, anno) in self.iter() {
			let mut sample = proto::Sample::default();

			let value = stack.weight.trunc();
			let value = i64::try_cast_from(value).expect("no exabyte heap sizes");
			sample.value.push(value);

			for addr in stack.addrs.iter().rev() {
				if *addr == 0 {
					continue;
				}
				// See the comment
				// [here](https://github.com/rust-lang/backtrace-rs/blob/036d4909e1fb9c08c2bb0f59ac81994e39489b2f/src/symbolize/mod.rs#L123-L147)
				// for why we need to subtract one. tl;dr addresses
				// in stack traces are actually the return address of
				// the called function, which is one past the call
				// itself.
				//
				// Of course, the `call` instruction can be more than one byte, so after subtracting
				// one, we might point somewhere in the middle of it, rather
				// than to the beginning of the instruction. That's fine; symbolization
				// tools don't seem to get confused by this.
				let addr = u64::cast_from(*addr) - 1;

				// Find the mapping for this address (search once)
				let mapping_info = self.mappings.iter().enumerate().find(|(_, mapping)| {
					mapping.memory_start <= addr as usize && mapping.memory_end > addr as usize
				});

				// Convert runtime address to file-relative address using found mapping data
				let file_relative_addr = mapping_info
					.map(|(_, mapping)| {
						(addr as usize - mapping.memory_start + mapping.file_offset as usize) as u64
					})
					.unwrap_or(addr);

				let loc_id = *location_ids.entry(file_relative_addr).or_insert_with(|| {
					// profile.proto says the location id may be the address, but Polar Signals
					// insists that location ids are sequential, starting with 1.
					let id = u64::cast_from(profile.location.len()) + 1;

					let mut mapping = mapping_info.and_then(|(idx, _)| profile.mapping.get_mut(idx));

					// If online symbolization is enabled, resolve the function and line.
					#[allow(unused_mut)]
					let mut line = Vec::new();

					backtrace::resolve(addr as *mut std::ffi::c_void, |symbol| {
						let Some(symbol_name) = symbol.name() else {
							return;
						};
						let function_name = format!("{symbol_name:#}");
						let lineno = symbol.lineno().unwrap_or(0) as i64;

						let function_id =
							*function_ids
								.entry(function_name)
								.or_insert_with_key(|function_name| {
									let function_id = profile.function.len() as u64 + 1;
									let system_name = String::from_utf8_lossy(symbol_name.as_bytes());
									let filename = symbol
										.filename()
										.map(|path| path.to_string_lossy())
										.unwrap_or(std::borrow::Cow::Borrowed(""));

									if let Some(ref mut mapping) = mapping {
										mapping.has_functions = true;
										mapping.has_filenames |= !filename.is_empty();
										mapping.has_line_numbers |= lineno > 0;
									}

									profile.function.push(proto::Function {
										id: function_id,
										name: strings.insert(function_name),
										system_name: strings.insert(&system_name),
										filename: strings.insert(&filename),
										..Default::default()
									});
									function_id
								});

						line.push(proto::Line {
							function_id,
							line: lineno,
						});

						if let Some(ref mut mapping) = mapping {
							mapping.has_inline_frames |= line.len() > 1;
						}
					});

					profile.location.push(proto::Location {
						id,
						mapping_id: mapping.map_or(0, |m| m.id),
						address: file_relative_addr,
						line,
						..Default::default()
					});
					id
				});

				sample.location_id.push(loc_id);

				if let Some(anno) = anno {
					sample.label.push(proto::Label {
						key: strings.insert(&anno_key),
						str: strings.insert(anno),
						..Default::default()
					})
				}
			}

			profile.sample.push(sample);
		}

		profile.string_table = strings.finish();

		profile
	}
}

pub struct StackProfileIter<'a> {
	inner: &'a StackProfile,
	idx: usize,
}

impl<'a> Iterator for StackProfileIter<'a> {
	type Item = (&'a WeightedStack, Option<&'a str>);

	fn next(&mut self) -> Option<Self::Item> {
		let (stack, anno) = self.inner.stacks.get(self.idx)?;
		self.idx += 1;
		let anno = anno.map(|idx| self.inner.annotations.get(idx).unwrap().as_str());
		Some((stack, anno))
	}
}

impl StackProfile {
	pub fn push_stack(&mut self, stack: WeightedStack, annotation: Option<&str>) {
		let anno_idx = if let Some(annotation) = annotation {
			Some(
				self
					.annotations
					.iter()
					.position(|anno| annotation == anno.as_str())
					.unwrap_or_else(|| {
						self.annotations.push(annotation.to_string());
						self.annotations.len() - 1
					}),
			)
		} else {
			None
		};
		self.stacks.push((stack, anno_idx))
	}

	pub fn push_mapping(&mut self, mapping: Mapping) {
		self.mappings.push(mapping);
	}

	pub fn iter(&self) -> StackProfileIter<'_> {
		StackProfileIter {
			inner: self,
			idx: 0,
		}
	}
}
