mod pprof;
pub mod stats;
mod trace;

use crate::pprof::{StackProfile, WeightedStack};
use crate::trace::HashedBacktrace;
use dashmap::DashMap;
use serde::Serialize;
use smallvec::smallvec;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::trace::CaptureMode;

pub struct PprofAlloc {
	inner: System,
	/// Enable profiling support
	pprof: bool,
	/// Enable coarse grained stats
	stats: bool,
}

#[derive(Clone)]
struct AllocationRecord {
	size: usize,
	trace: HashedBacktrace,
}

struct HeapSampleValues {
	alloc_space: i64,
	inuse_space: i64,
}

impl HeapSampleValues {
	fn from_allocations(stats: &stats::Allocations) -> Self {
		Self {
			alloc_space: stats.allocated as i64,
			inuse_space: stats.in_use_bytes() as i64,
		}
	}
}

#[derive(Clone, Debug, Serialize)]
pub struct Probe<T> {
	pub value: Option<T>,
	pub error: Option<String>,
}

impl<T> Probe<T> {
	fn ok(value: T) -> Self {
		Self {
			value: Some(value),
			error: None,
		}
	}

	fn err(error: impl ToString) -> Self {
		Self {
			value: None,
			error: Some(error.to_string()),
		}
	}
}

impl<T, E> From<Result<T, E>> for Probe<T>
where
	E: std::fmt::Display,
{
	fn from(result: Result<T, E>) -> Self {
		match result {
			Ok(value) => Self::ok(value),
			Err(error) => Self::err(error),
		}
	}
}

#[derive(Clone, Debug, Serialize)]
pub struct MallocSummary {
	pub system_max: u64,
	pub system_current: u64,
	pub total: u64,
	pub heaps: u64,
}

impl From<&stats::malloc::MallocInfo> for MallocSummary {
	fn from(info: &stats::malloc::MallocInfo) -> Self {
		Self {
			system_max: info.system_max(),
			system_current: info.system_current(),
			total: info.total(),
			heaps: info.heaps(),
		}
	}
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PprofSummary {
	pub total_stacks: u64,
	pub live_stacks: u64,
	pub alloc_space_bytes: u64,
	pub inuse_space_bytes: u64,
	pub alloc_objects: u64,
	pub inuse_objects: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemorySnapshot {
	pub captured_at_unix_ms: u64,
	pub capture_mode: CaptureMode,
	pub allocation_stats: stats::Allocations,
	pub pprof: PprofSummary,
	pub malloc: Probe<MallocSummary>,
	pub cgroup: Probe<stats::cgroups::MemoryStat>,
	pub smaps: Probe<stats::smaps::ProcessStats>,
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

	fn record_allocation(&self, ptr: usize, size: usize) {
		if self.stats {
			GLOBAL_STATS
				.allocated
				.fetch_add(size as u64, Ordering::Relaxed);
			GLOBAL_STATS.allocations.fetch_add(1, Ordering::Relaxed);
		}

		if !self.pprof {
			return;
		}

		let trace = HashedBacktrace::capture();
		self.record_allocation_with_trace(ptr, size, trace);
	}

	fn record_allocation_with_trace(&self, ptr: usize, size: usize, trace: HashedBacktrace) {
		POINTER_MAP.insert(
			ptr,
			AllocationRecord {
				size,
				trace: trace.clone(),
			},
		);
		let mut stats = TRACE_MAP.entry(trace).or_default();
		stats.allocated += size as u64;
		stats.allocations += 1;
	}

	fn record_deallocation(&self, ptr: usize, size: usize) {
		let record = if self.pprof {
			POINTER_MAP.remove(&ptr).map(|(_, record)| record)
		} else {
			None
		};
		let freed_size = record.as_ref().map(|record| record.size).unwrap_or(size);

		if self.stats {
			GLOBAL_STATS
				.freed
				.fetch_add(freed_size as u64, Ordering::Relaxed);
			GLOBAL_STATS.frees.fetch_add(1, Ordering::Relaxed);
		}

		let Some(record) = record else {
			return;
		};

		let mut stats = TRACE_MAP.entry(record.trace).or_default();
		stats.freed += freed_size as u64;
		stats.frees += 1;
	}

	fn record_reallocation(&self, old_ptr: usize, old_size: usize, new_ptr: usize, new_size: usize) {
		self.record_deallocation(old_ptr, old_size);
		self.record_allocation(new_ptr, new_size);
	}
}

fn enter_alloc<T>(func: impl FnOnce() -> T) -> T {
	let current_value = IN_ALLOC.with(|x| x.get());
	IN_ALLOC.with(|x| x.set(true));
	let output = func();
	IN_ALLOC.with(|x| x.set(current_value));
	output
}

thread_local! {
	/// Used to avoid recursive alloc/dealloc calls for interior allocation.
	static IN_ALLOC: Cell<bool> = const { Cell::new(false) };
}

static GLOBAL_STATS: stats::AtomicAllocations = stats::AtomicAllocations::new();

lazy_static::lazy_static! {
	static ref POINTER_MAP: DashMap<usize, AllocationRecord> = DashMap::new();
	static ref TRACE_MAP: DashMap<HashedBacktrace, stats::Allocations> = DashMap::new();
}

pub fn allocation_stats() -> stats::Allocations {
	GLOBAL_STATS.snapshot()
}

pub const fn capture_mode() -> CaptureMode {
	trace::capture_mode()
}

pub fn snapshot() -> MemorySnapshot {
	enter_alloc(|| MemorySnapshot {
		captured_at_unix_ms: SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.expect("system time must be after the UNIX epoch")
			.as_millis()
			.try_into()
			.expect("timestamp must fit in u64"),
		capture_mode: capture_mode(),
		allocation_stats: allocation_stats(),
		pprof: pprof_summary(),
		malloc: Probe::from(stats::malloc::info().map(|info| MallocSummary::from(&info))),
		cgroup: Probe::from(stats::cgroups::get_stats()),
		smaps: Probe::from(stats::smaps::rollup()),
	})
}

fn pprof_summary() -> PprofSummary {
	let mut summary = PprofSummary::default();
	for entry in TRACE_MAP.iter() {
		let stats = entry.value();
		summary.total_stacks += 1;
		summary.alloc_space_bytes += stats.allocated;
		summary.inuse_space_bytes += stats.in_use_bytes();
		summary.alloc_objects += stats.allocations;
		summary.inuse_objects += stats.in_use_allocations();
		if stats.in_use_bytes() > 0 {
			summary.live_stacks += 1;
		}
	}
	summary
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

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		unsafe {
			if IN_ALLOC.with(|x| x.get()) {
				return self.inner.alloc_zeroed(layout);
			}

			enter_alloc(|| {
				let ptr = self.inner.alloc_zeroed(layout);
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

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		unsafe {
			if IN_ALLOC.with(|x| x.get()) {
				return self.inner.realloc(ptr, layout, new_size);
			}

			enter_alloc(|| {
				let new_ptr = self.inner.realloc(ptr, layout, new_size);
				if !new_ptr.is_null() {
					self.record_reallocation(ptr as usize, layout.size(), new_ptr as usize, new_size);
				}
				new_ptr
			})
		}
	}
}

pub fn generate_pprof() -> anyhow::Result<Vec<u8>> {
	enter_alloc(|| {
		let mut profile = StackProfile {
			annotations: Default::default(),
			stacks: Default::default(),
			mappings: if let Some(m) = crate::pprof::MAPPINGS.as_deref() {
				m.to_vec()
			} else {
				Default::default()
			},
		};

		for entry in TRACE_MAP.iter() {
			let sample_values = HeapSampleValues::from_allocations(entry.value());
			if sample_values.alloc_space == 0 && sample_values.inuse_space == 0 {
				continue;
			}

			profile.push_stack(
				WeightedStack {
					addrs: entry.key().addrs(),
					values: smallvec![sample_values.alloc_space, sample_values.inuse_space],
				},
				None,
			);
		}

		Ok(profile.to_pprof(
			&[("alloc_space", "bytes"), ("inuse_space", "bytes")],
			("space", "bytes"),
			None,
		))
	})
}

#[cfg(test)]
fn reset_tracking_state() {
	POINTER_MAP.clear();
	TRACE_MAP.clear();
	GLOBAL_STATS.allocated.store(0, Ordering::Relaxed);
	GLOBAL_STATS.freed.store(0, Ordering::Relaxed);
	GLOBAL_STATS.allocations.store(0, Ordering::Relaxed);
	GLOBAL_STATS.frees.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
	use super::*;
	use parking_lot::Mutex;

	static TEST_GUARD: Mutex<()> = Mutex::new(());

	#[test]
	fn allocation_stats_compute_in_use_values() {
		let stats = stats::Allocations {
			allocated: 4096,
			freed: 1024,
			allocations: 4,
			frees: 1,
		};

		assert_eq!(stats.in_use_bytes(), 3072);
		assert_eq!(stats.in_use_allocations(), 3);
	}

	#[test]
	fn deallocation_updates_live_profile_bytes() {
		let _guard = TEST_GUARD.lock();
		reset_tracking_state();

		let alloc = PprofAlloc::new().with_pprof().with_stats();
		let trace = HashedBacktrace::capture();

		alloc.record_allocation_with_trace(0x1000, 128, trace.clone());
		alloc.record_allocation_with_trace(0x2000, 64, trace.clone());
		alloc.record_deallocation(0x1000, 128);

		let trace_stats = TRACE_MAP.get(&trace).unwrap();
		assert_eq!(trace_stats.allocated, 192);
		assert_eq!(trace_stats.freed, 128);
		assert_eq!(trace_stats.allocations, 2);
		assert_eq!(trace_stats.frees, 1);
		assert_eq!(trace_stats.in_use_bytes(), 64);
		assert!(POINTER_MAP.contains_key(&0x2000));
		assert!(!POINTER_MAP.contains_key(&0x1000));
	}

	#[test]
	fn coarse_stats_track_allocations_and_frees() {
		let _guard = TEST_GUARD.lock();
		reset_tracking_state();

		let alloc = PprofAlloc::new().with_stats();
		alloc.record_allocation(0x5000, 48);
		alloc.record_deallocation(0x5000, 48);

		assert_eq!(
			allocation_stats(),
			stats::Allocations {
				allocated: 48,
				freed: 48,
				allocations: 1,
				frees: 1,
			}
		);
	}

	#[test]
	fn reallocation_updates_live_bytes_and_pointer_ownership() {
		let _guard = TEST_GUARD.lock();
		reset_tracking_state();

		let alloc = PprofAlloc::new().with_pprof();
		let original_trace = HashedBacktrace::capture();

		alloc.record_allocation_with_trace(0x3000, 32, original_trace.clone());
		alloc.record_reallocation(0x3000, 32, 0x4000, 96);

		let total_live_bytes: u64 = TRACE_MAP
			.iter()
			.map(|entry| entry.value().in_use_bytes())
			.sum();
		assert_eq!(TRACE_MAP.get(&original_trace).unwrap().in_use_bytes(), 0);
		assert_eq!(total_live_bytes, 96);
		assert_eq!(POINTER_MAP.get(&0x4000).unwrap().size, 96);
	}

	#[test]
	fn snapshot_reports_current_pprof_summary() {
		let _guard = TEST_GUARD.lock();
		reset_tracking_state();

		let alloc = PprofAlloc::new().with_pprof();
		let trace = HashedBacktrace::capture();

		alloc.record_allocation_with_trace(0x1000, 128, trace.clone());
		alloc.record_allocation_with_trace(0x2000, 64, trace);
		alloc.record_deallocation(0x1000, 128);

		let snapshot = snapshot();
		assert_eq!(snapshot.capture_mode, CaptureMode::FramePointer);
		assert_eq!(
			snapshot.pprof,
			PprofSummary {
				total_stacks: 1,
				live_stacks: 1,
				alloc_space_bytes: 192,
				inuse_space_bytes: 64,
				alloc_objects: 2,
				inuse_objects: 1,
			}
		);
	}
}
