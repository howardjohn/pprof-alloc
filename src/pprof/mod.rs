mod cast;
mod mappings;
#[path = "perftools.profiles.rs"]
mod proto;

use std::collections::BTreeMap;
use std::fmt;
#[cfg(feature = "allocator-jemalloc")]
use std::io::BufRead;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "allocator-jemalloc")]
use anyhow::bail;
use flate2::Compression;
use flate2::write::GzEncoder;
use prost::Message;
use smallvec::SmallVec;

pub use cast::CastFrom;
pub use mappings::MAPPINGS;

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
	pub values: SmallVec<[i64; 4]>,
}

/// A mapping of a single shared object.
#[derive(Clone, Debug)]
pub struct Mapping {
	pub memory_start: usize,
	pub memory_end: usize,
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
	pub fn to_pprof_with_period(
		&self,
		sample_types: &[(&str, &str)],
		period_type: (&str, &str),
		period: i64,
		anno_key: Option<String>,
	) -> Vec<u8> {
		let profile = self.to_pprof_proto(sample_types, period_type, period, anno_key);
		let encoded = profile.encode_to_vec();

		let mut gz = GzEncoder::new(Vec::new(), Compression::default());
		gz.write_all(&encoded).unwrap();
		gz.finish().unwrap()
	}

	/// Converts the profile into the pprof Protobuf format (see `pprof/profile.proto`).
	fn to_pprof_proto(
		&self,
		sample_types: &[(&str, &str)],
		period_type: (&str, &str),
		period: i64,
		anno_key: Option<String>,
	) -> proto::Profile {
		assert!(
			!sample_types.is_empty(),
			"pprof needs at least one sample type"
		);

		let mut profile = proto::Profile::default();
		profile.sample.reserve(self.stacks.len());
		let mut strings = StringTable::new();

		let anno_key = anno_key.unwrap_or_else(|| "annotation".into());

		profile.sample_type = sample_types
			.iter()
			.map(|(sample_type, unit)| proto::ValueType {
				r#type: strings.insert(sample_type),
				unit: strings.insert(unit),
			})
			.collect();
		profile.period_type = Some(proto::ValueType {
			r#type: strings.insert(period_type.0),
			unit: strings.insert(period_type.1),
		});
		profile.period = period;
		profile.default_sample_type = strings.insert(sample_types.last().unwrap().0);

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
			assert_eq!(
				stack.values.len(),
				sample_types.len(),
				"each sample must provide one value per sample type"
			);
			sample.value.extend_from_slice(&stack.values);

			for addr in &stack.addrs {
				if *addr == 0 {
					continue;
				}
				// Stack capture already records addresses in leaf-to-root order and normalizes
				// caller return PCs into an instruction address. Preserve that order here because
				// profile.proto expects location_id[0] to be the leaf frame.
				let addr = u64::cast_from(*addr);
				if is_pprof_alloc_internal_frame(addr) {
					continue;
				}

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

#[cfg(feature = "allocator-jemalloc")]
fn scale_jemalloc_heap_sample(objects: f64, bytes: f64, sampling_rate: f64) -> (i64, i64) {
	if objects == 0.0 || bytes == 0.0 {
		return (0, 0);
	}

	let ratio = (bytes / objects) / sampling_rate;
	let scale_factor = 1.0 / (1.0 - (-ratio).exp());
	let scaled_objects = (objects * scale_factor).trunc();
	let scaled_bytes = (bytes * scale_factor).trunc();
	(
		scaled_objects.clamp(i64::MIN as f64, i64::MAX as f64) as i64,
		scaled_bytes.clamp(i64::MIN as f64, i64::MAX as f64) as i64,
	)
}

/// Parse jemalloc's textual `heap_v2` profile format into this crate's pprof model.
#[cfg(feature = "allocator-jemalloc")]
pub(crate) fn parse_jemalloc_heap_profile<R: BufRead>(
	r: R,
	mappings: Vec<Mapping>,
) -> anyhow::Result<StackProfile> {
	let mut current_stack = None;
	let mut profile = StackProfile {
		annotations: Default::default(),
		stacks: Default::default(),
		mappings,
	};
	let mut lines = r.lines();

	let first_line = match lines.next() {
		Some(line) => line?,
		None => bail!("jemalloc heap dump was empty"),
	};
	let sampling_rate: f64 = first_line
		.trim()
		.strip_prefix("heap_v2/")
		.ok_or_else(|| anyhow::anyhow!("unsupported jemalloc heap profile header: {first_line}"))?
		.parse()?;

	for line in lines {
		let line = line?;
		let words: Vec<_> = line.split_ascii_whitespace().collect();
		if words.is_empty() {
			continue;
		}

		if words[0] == "@" {
			if current_stack.is_some() {
				bail!("jemalloc heap dump contains a stack without counters");
			}
			let addrs = words[1..]
				.iter()
				.map(|word| {
					let raw = word.trim_start_matches("0x");
					let addr = u64::from_str_radix(raw, 16)?;
					Ok(addr.saturating_sub(1))
				})
				.collect::<Result<Vec<_>, std::num::ParseIntError>>()?;
			current_stack = Some(addrs);
		} else if words.len() > 2 && words[0] == "t*:" {
			let Some(addrs) = current_stack.take() else {
				continue;
			};

			let current_objects: f64 = words[1].trim_end_matches(':').parse()?;
			let current_bytes: f64 = words[2].parse()?;
			let accumulated_objects: f64 = words
				.get(3)
				.and_then(|word| word.strip_prefix('['))
				.unwrap_or("0")
				.trim_end_matches(':')
				.parse()?;
			let accumulated_bytes: f64 = words
				.get(4)
				.map(|word| word.trim_end_matches(']'))
				.unwrap_or("0")
				.parse()?;

			let (inuse_objects, inuse_space) =
				scale_jemalloc_heap_sample(current_objects, current_bytes, sampling_rate);
			let (alloc_objects, alloc_space) =
				scale_jemalloc_heap_sample(accumulated_objects, accumulated_bytes, sampling_rate);
			if inuse_objects == 0 && inuse_space == 0 && alloc_objects == 0 && alloc_space == 0 {
				continue;
			}

			profile.push_stack(
				WeightedStack {
					addrs,
					values: smallvec::smallvec![alloc_objects, alloc_space, inuse_objects, inuse_space],
				},
				None,
			);
		}
	}

	if current_stack.is_some() {
		bail!("jemalloc heap dump contains a stack without counters");
	}

	Ok(profile)
}

fn is_pprof_alloc_internal_frame(addr: u64) -> bool {
	let mut internal = false;
	backtrace::resolve(addr as *mut std::ffi::c_void, |symbol| {
		let Some(symbol_name) = symbol.name() else {
			return;
		};
		let function_name = format!("{symbol_name:#}");
		internal |= is_pprof_alloc_internal_function(&function_name);
	});
	internal
}

fn is_pprof_alloc_internal_function(function_name: &str) -> bool {
	function_name.starts_with("pprof_alloc::")
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

	pub fn iter(&self) -> StackProfileIter<'_> {
		StackProfileIter {
			inner: self,
			idx: 0,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use flate2::read::GzDecoder;
	use smallvec::smallvec;
	use std::io::Read;

	#[test]
	fn profile_can_emit_alloc_and_inuse_sample_types() {
		let mut profile = StackProfile::default();
		profile.push_stack(
			WeightedStack {
				addrs: vec![0x1000],
				values: smallvec![42, 7],
			},
			None,
		);

		let encoded = profile.to_pprof_with_period(
			&[("alloc_space", "bytes"), ("inuse_space", "bytes")],
			("space", "bytes"),
			0,
			None,
		);

		let mut decoder = GzDecoder::new(encoded.as_slice());
		let mut decoded_bytes = Vec::new();
		decoder.read_to_end(&mut decoded_bytes).unwrap();

		let decoded = proto::Profile::decode(decoded_bytes.as_slice()).unwrap();
		assert_eq!(decoded.sample_type.len(), 2);
		assert_eq!(decoded.sample.len(), 1);
		assert_eq!(decoded.sample[0].value, vec![42, 7]);
		assert_eq!(
			decoded.string_table[decoded.default_sample_type as usize],
			"inuse_space"
		);
	}

	#[test]
	fn profile_preserves_leaf_to_root_stack_order() {
		let mut profile = StackProfile::default();
		profile.push_stack(
			WeightedStack {
				addrs: vec![0x1000, 0x2000, 0x3000],
				values: smallvec![1],
			},
			None,
		);

		let encoded =
			profile.to_pprof_with_period(&[("inuse_space", "bytes")], ("space", "bytes"), 0, None);

		let mut decoder = GzDecoder::new(encoded.as_slice());
		let mut decoded_bytes = Vec::new();
		decoder.read_to_end(&mut decoded_bytes).unwrap();

		let decoded = proto::Profile::decode(decoded_bytes.as_slice()).unwrap();
		let sample = &decoded.sample[0];
		let by_id = decoded
			.location
			.iter()
			.map(|location| (location.id, location.address))
			.collect::<BTreeMap<_, _>>();
		let sample_addrs = sample
			.location_id
			.iter()
			.map(|id| *by_id.get(id).unwrap())
			.collect::<Vec<_>>();

		assert_eq!(sample_addrs, vec![0x1000, 0x2000, 0x3000]);
	}

	#[test]
	fn pprof_alloc_internal_function_names_are_filtered() {
		assert!(is_pprof_alloc_internal_function(
			"pprof_alloc::trace::frame_pointer::trace"
		));
		assert!(is_pprof_alloc_internal_function(
			"pprof_alloc::trace::backtrace::trace"
		));
		assert!(!is_pprof_alloc_internal_function(
			"example::allocation_hot_path"
		));
	}

	#[test]
	#[cfg(feature = "allocator-jemalloc")]
	fn jemalloc_heap_profile_parser_reads_current_and_accumulated_counters() {
		let input = b"heap_v2/512\n@ 0x20 0x10\n  t*: 1: 1024 [3: 2048]\n";
		let profile = parse_jemalloc_heap_profile(&input[..], Vec::new()).unwrap();

		assert_eq!(profile.stacks.len(), 1);
		let stack = &profile.stacks[0].0;
		assert_eq!(stack.addrs, vec![0x1f, 0xf]);
		assert_eq!(stack.values.len(), 4);
		assert!((4..=5).contains(&stack.values[0]));
		assert!((2770..=2790).contains(&stack.values[1]));
		assert_eq!(stack.values[2], 1);
		assert!((1180..=1190).contains(&stack.values[3]));
	}
}
