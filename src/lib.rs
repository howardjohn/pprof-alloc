mod pprof;
pub mod stats;
mod trace;

use crate::pprof::{StackProfile, WeightedStack};
use crate::trace::HashedBacktrace;
use dashmap::DashMap;
use itertools::Itertools;
use parking_lot::Mutex;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

struct TheadMap<V> {
	shards: Box<[Mutex<prehash::PrehashedMap<u64, V>>]>,
}

impl<V> TheadMap<V> {
	pub fn new(size: usize) -> Self {
		Self {
			shards: (0..size + 1)
				.map(|_| Mutex::new(prehash::PrehashedMap::<u64, V>::default()))
				.collect(),
		}
	}

	fn update(&self, thread: usize, key: u64, def: impl FnOnce() -> V, apply: impl FnOnce(&mut V)) {
		// Use actual thread ID as shard
		let shard_idx = if thread < self.shards.len() {
			thread
		} else {
			self.shards.len() - 1
		};
		let k = prehash::Prehashed::new(key, key);
		let mut l = self.shards[shard_idx].lock();
		let v = l.entry(k).or_insert_with(def);
		apply(v);
	}

	fn iter(&self, mut def: impl FnMut(&V)) {
		for shard in &self.shards {
			shard.lock().iter().for_each(|(_, v)| def(v));
		}
	}
}

pub struct PprofAlloc {
	inner: System,
	/// Enable profiling support
	pprof: bool,
	/// Enable coarse grained stats
	stats: bool,
}

impl Default for PprofAlloc {
	fn default() -> Self {
		Self::new()
	}
}

impl PprofAlloc {
	pub const fn new() -> Self {
		PprofAlloc {
			inner: System,
			pprof: false,
			stats: false,
		}
	}
	pub const fn with_pprof(mut self) -> Self {
		self.pprof = true;
		self
	}
	pub const fn with_stats(mut self) -> Self {
		self.stats = true;
		self
	}
}

fn enter_alloc<T>(func: impl FnOnce() -> T) -> T {
	let current_value = IN_ALLOC.with(|x| x.get());
	IN_ALLOC.with(|x| x.set(true));
	let output = func();
	IN_ALLOC.with(|x| x.set(current_value));
	output
}

/// next thread id incrementor
static THREAD_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
thread_local! {
		static THREAD_ID: usize = THREAD_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
		static THREAD_NAME: Option<Arc<str>> = std::thread::current().name().map(Arc::from);
		/// Used to avoid recursive alloc/dealloc calls for interior allocation
		static IN_ALLOC: Cell<bool> = const { Cell::new(false) };
}
static GLOBAL_STATS: stats::AtomicAllocations = stats::AtomicAllocations::new();

lazy_static::lazy_static! {
		/// pointer -> size
		static ref POINTER_MAP: DashMap<usize, usize> = DashMap::new();
		static ref LEAKY_POINTER_MAP: DashMap<usize, usize> = DashMap::new();
		// backtrace -> current allocation size
		static ref TRACE_MAP: TheadMap<TraceInfo> = TheadMap::new(64);
}

pub struct TraceInfo {
	pub backtrace: HashedBacktrace,
	pub stats: stats::Allocations,
}

fn thread_id() -> usize {
	THREAD_ID.with(|id| *id)
}

fn thread_name() -> (usize, Arc<str>) {
	(
		THREAD_ID.with(|id| *id),
		THREAD_NAME.with(|n| n.clone()).unwrap_or_default(),
	)
}

unsafe impl GlobalAlloc for PprofAlloc {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		unsafe {
			if IN_ALLOC.with(|x| x.get()) {
				return self.inner.alloc(layout);
			}
			enter_alloc(|| {
				let ptr = self.inner.alloc(layout);
				if !ptr.is_null() {
					self.record_allocation(ptr as usize, layout.size());
				}
				ptr
			})
		}
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe {
			if IN_ALLOC.with(|x| x.get()) {
				self.inner.dealloc(ptr, layout);
				return;
			}
			enter_alloc(|| {
				self.inner.dealloc(ptr, layout);
				self.record_deallocation(ptr as usize, layout.size());
			});
		}
	}
}

static ALLOCATIONS: LazyLock<Mutex<HashMap<String, usize>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

impl PprofAlloc {
	fn record_allocation(&self, start: usize, size: usize) {
		let id = thread_id();

		if self.stats {
			GLOBAL_STATS
				.allocated
				.fetch_add(size as u64, Ordering::Relaxed);
			GLOBAL_STATS.allocations.fetch_add(1, Ordering::Relaxed);
		}

		if self.pprof {
			let trace = crate::trace::HashedBacktrace::capture();

			// POINTER_MAP.entry(start).insert(size);
			// LEAKY_POINTER_MAP.entry(start).insert(size);

			TRACE_MAP.update(
				id,
				trace.hash(),
				|| TraceInfo {
					backtrace: trace,
					stats: Default::default(),
				},
				|i| {
					i.stats.allocated += size as u64;
					i.stats.allocations += 1;
				},
			);
		}
		// let bt = Backtrace::new();
		// let key = format!("{:?}", bt);
		// let mut map = ALLOCATIONS.lock().unwrap();
		// *map.entry(key).or_insert(0) += size;
	}

	fn record_deallocation(&self, start: usize, size: usize) {
		if self.stats {
			GLOBAL_STATS.freed.fetch_add(size as u64, Ordering::Relaxed);
			GLOBAL_STATS.frees.fetch_add(1, Ordering::Relaxed);
		}
		return;
		POINTER_MAP.remove(&start);
		// TODO: TRACE_MAP
	}
}

pub fn generate_pprof() -> anyhow::Result<Vec<u8>> {
	IN_ALLOC.with(|x| x.set(true));
	let mut profile = StackProfile {
		annotations: Default::default(),
		stacks: Default::default(),
		mappings: if let Some(m) = crate::pprof::MAPPINGS.as_deref() {
			m.to_vec()
		} else {
			Default::default()
		},
	};
	// let sampling_rate: f64 = 1.0;
	TRACE_MAP.iter(|entry| {
		let addrs = entry.backtrace.addrs();
		let weight = entry.stats.allocated as f64;
		profile.push_stack(WeightedStack { addrs, weight }, None);
	});
	IN_ALLOC.with(|x| x.set(false));
	let pprof = profile.to_pprof(("inuse_space", "bytes"), ("space", "bytes"), None);
	Ok(pprof)
}
