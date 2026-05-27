//! Allocation profiling and Linux memory telemetry for Rust services.
//!
//! `pprof-alloc` provides a [`GlobalAlloc`] wrapper that can sample allocation
//! stack traces and export them as gzipped pprof heap profiles. It also exposes
//! Linux memory collectors for allocator state, cgroup v2 memory accounting, and
//! `/proc/self/smaps_rollup` process residency.
//!
//! The crate is intended to be embedded in binaries that expose their own debug
//! or metrics endpoint. Use [`PprofAlloc`] as the process global allocator, then
//! call [`generate_pprof`] or [`snapshot`] from your application surface.
//!
//! [`GlobalAlloc`]: std::alloc::GlobalAlloc

pub mod allocator;
mod pprof;
pub mod stats;
mod trace;

use crate::pprof::{StackProfile, WeightedStack};
use crate::trace::HashedBacktrace;
use dashmap::DashMap;
use serde::Serialize;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::trace::CaptureMode;

/// Default average number of allocated bytes between recorded pprof samples.
///
/// This matches Go's default heap profiling rate: one sampled allocation per
/// 512 KiB of allocated bytes on average. A rate of `1` records every
/// allocation, while `0` disables pprof stack recording.
pub const DEFAULT_PPROF_SAMPLE_RATE: usize = 512 * 1024;

/// Environment variable read by [`PprofAlloc::with_pprof_sample_rate_from_env`].
///
/// The value must be an unsigned integer byte rate. Missing or invalid values
/// use the default passed to `with_pprof_sample_rate_from_env`.
pub const PPROF_SAMPLE_RATE_ENV: &str = "PPROF_ALLOC_SAMPLE_RATE";

const PPROF_SAMPLE_RATE_ENV_CSTR: &[u8] = b"PPROF_ALLOC_SAMPLE_RATE\0";
const MAX_FAST_EXP_RAND_MEAN: usize = 0x7000000;
const RANDOM_BIT_COUNT: u32 = 26;
const ENV_SAMPLE_RATE_UNINITIALIZED: u8 = 0;
const ENV_SAMPLE_RATE_SET: u8 = 1;
const ENV_SAMPLE_RATE_UNSET: u8 = 2;
const RESOLVED_SAMPLE_RATE_UNINITIALIZED: usize = usize::MAX;
const STATS_FLUSH_EVENTS: u64 = 1024;
const STATS_FLUSH_BYTES: u64 = 1024 * 1024;

/// Global allocator wrapper that can collect allocation counters and pprof heap profiles.
///
/// Use this as a `#[global_allocator]` around [`std::alloc::System`] or another
/// allocator implementing [`GlobalAlloc`]. Profiling and coarse allocation
/// counters are opt-in through the builder methods.
pub struct PprofAlloc<A = System> {
	inner: A,
	/// Enable profiling support
	pprof: bool,
	/// Enable coarse grained stats
	stats: bool,
	/// Average bytes between pprof samples. 0 disables pprof, 1 records everything.
	pprof_sample_rate: usize,
	/// Read the pprof sample rate from PPROF_ALLOC_SAMPLE_RATE at runtime.
	pprof_sample_rate_from_env: bool,
	/// Cached resolved sample rate for env-driven configuration.
	resolved_pprof_sample_rate: AtomicUsize,
}

#[derive(Clone)]
struct AllocationRecord {
	size: usize,
	trace: HashedBacktrace,
}

struct HeapSampleValues {
	alloc_objects: i64,
	alloc_space: i64,
	inuse_objects: i64,
	inuse_space: i64,
}

struct LocalAllocationStats {
	allocated: Cell<u64>,
	freed: Cell<u64>,
	allocations: Cell<u64>,
	frees: Cell<u64>,
}

impl LocalAllocationStats {
	const fn new() -> Self {
		Self {
			allocated: Cell::new(0),
			freed: Cell::new(0),
			allocations: Cell::new(0),
			frees: Cell::new(0),
		}
	}

	fn record_allocation(&self, size: u64) {
		self
			.allocated
			.set(self.allocated.get().saturating_add(size));
		self
			.allocations
			.set(self.allocations.get().saturating_add(1));
		self.flush_if_needed();
	}

	fn record_deallocation(&self, size: u64) {
		self.freed.set(self.freed.get().saturating_add(size));
		self.frees.set(self.frees.get().saturating_add(1));
		self.flush_if_needed();
	}

	fn flush_if_needed(&self) {
		let events = self.allocations.get().saturating_add(self.frees.get());
		let bytes = self.allocated.get().saturating_add(self.freed.get());
		if events >= STATS_FLUSH_EVENTS || bytes >= STATS_FLUSH_BYTES {
			self.flush();
		}
	}

	fn flush(&self) {
		let allocated = self.allocated.replace(0);
		let freed = self.freed.replace(0);
		let allocations = self.allocations.replace(0);
		let frees = self.frees.replace(0);

		if allocated != 0 {
			GLOBAL_STATS
				.allocated
				.fetch_add(allocated, Ordering::Relaxed);
		}
		if freed != 0 {
			GLOBAL_STATS.freed.fetch_add(freed, Ordering::Relaxed);
		}
		if allocations != 0 {
			GLOBAL_STATS
				.allocations
				.fetch_add(allocations, Ordering::Relaxed);
		}
		if frees != 0 {
			GLOBAL_STATS.frees.fetch_add(frees, Ordering::Relaxed);
		}
	}

	#[cfg(test)]
	fn reset(&self) {
		self.allocated.set(0);
		self.freed.set(0);
		self.allocations.set(0);
		self.frees.set(0);
	}
}

impl Drop for LocalAllocationStats {
	fn drop(&mut self) {
		self.flush();
	}
}

impl HeapSampleValues {
	fn from_allocations(stats: &stats::Allocations, sample_rate: usize) -> Self {
		let (alloc_objects, alloc_space) =
			scale_heap_sample(stats.allocations, stats.allocated, sample_rate);
		let (inuse_objects, inuse_space) = scale_heap_sample(
			stats.in_use_allocations(),
			stats.in_use_bytes(),
			sample_rate,
		);
		Self {
			alloc_objects,
			alloc_space,
			inuse_objects,
			inuse_space,
		}
	}
}

fn saturating_i64(value: u64) -> i64 {
	value.min(i64::MAX as u64) as i64
}

fn scale_heap_sample(count: u64, size: u64, sample_rate: usize) -> (i64, i64) {
	if count == 0 || size == 0 {
		return (0, 0);
	}

	if sample_rate <= 1 {
		return (saturating_i64(count), saturating_i64(size));
	}

	let average_size = size as f64 / count as f64;
	let probability = -(-average_size / sample_rate as f64).exp_m1();
	if probability <= 0.0 {
		return (saturating_i64(count), saturating_i64(size));
	}
	let scale = 1.0 / probability;
	(
		(count as f64 * scale).min(i64::MAX as f64) as i64,
		(size as f64 * scale).min(i64::MAX as f64) as i64,
	)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
/// Summary of the currently recorded pprof allocation profile.
///
/// Values are scaled according to the active pprof sample rate, so they are
/// estimates when sampling is enabled.
pub struct PprofSummary {
	/// Number of distinct stack traces recorded in the profile.
	pub total_stacks: u64,
	/// Number of distinct stack traces with estimated live bytes greater than zero.
	pub live_stacks: u64,
	/// Estimated cumulative allocated bytes across all recorded stacks.
	pub alloc_space_bytes: u64,
	/// Estimated currently live bytes across all recorded stacks.
	pub inuse_space_bytes: u64,
	/// Estimated cumulative allocation count across all recorded stacks.
	pub alloc_objects: u64,
	/// Estimated currently live allocation count across all recorded stacks.
	pub inuse_objects: u64,
}

#[derive(Clone, Debug, Serialize)]
/// Best-effort snapshot of allocation and memory state for the current process created by `snapshot()`.
///
/// Snapshot collection never fails as a whole. Optional fields are `None` when
/// the corresponding operating-system or allocator probe is unavailable.
pub struct MemorySnapshot {
	/// Wall-clock capture time as milliseconds since the Unix epoch.
	pub captured_at_unix_ms: u64,
	/// Stack capture implementation compiled into this build.
	pub capture_mode: CaptureMode,
	/// Coarse process-wide counters from [`allocation_stats`].
	pub allocation_stats: stats::Allocations,
	/// Summary of recorded pprof stack-attributed allocation data.
	pub pprof: PprofSummary,
	/// Active allocator identity and allocator-specific memory stats, if available.
	pub allocator: allocator::AllocatorSnapshot,
	/// cgroup v2 memory stats for this process, when available.
	pub cgroup: Option<stats::cgroups::MemoryStat>,
	/// `/proc/self/smaps_rollup` memory stats for this process, when available.
	pub smaps: Option<stats::smaps::ProcessStats>,
}

impl Default for PprofAlloc<System> {
	fn default() -> Self {
		Self::new()
	}
}

impl PprofAlloc<System> {
	/// Create a wrapper around [`std::alloc::System`] with profiling disabled.
	///
	/// Use [`Self::with_pprof`], [`Self::with_pprof_sample_rate`], or
	/// [`Self::with_stats`] to enable collection.
	pub const fn new() -> Self {
		Self::from_allocator(System)
	}
}

impl<A> PprofAlloc<A> {
	/// Create a wrapper around a custom allocator with profiling disabled.
	pub const fn from_allocator(inner: A) -> Self {
		PprofAlloc {
			inner,
			pprof: false,
			stats: false,
			pprof_sample_rate: DEFAULT_PPROF_SAMPLE_RATE,
			pprof_sample_rate_from_env: false,
			resolved_pprof_sample_rate: AtomicUsize::new(RESOLVED_SAMPLE_RATE_UNINITIALIZED),
		}
	}

	/// Enable sampled pprof stack profiling using [`DEFAULT_PPROF_SAMPLE_RATE`].
	pub const fn with_pprof(mut self) -> Self {
		self.pprof = true;
		self
	}

	/// Enable sampled pprof stack profiling with an explicit byte sample rate.
	///
	/// A rate of `1` records every allocation. A rate of `0` disables pprof
	/// stack recording while still allowing other enabled collectors to run.
	pub const fn with_pprof_sample_rate(mut self, bytes: usize) -> Self {
		self.pprof = true;
		self.pprof_sample_rate = bytes;
		self.pprof_sample_rate_from_env = false;
		self
	}

	/// Enable pprof stack profiling with the sample rate read from the environment.
	///
	/// [`PPROF_SAMPLE_RATE_ENV`] is read lazily on the first profiled allocation.
	/// If the variable is missing or invalid, `default_rate` is used.
	pub const fn with_pprof_sample_rate_from_env(mut self, default_rate: usize) -> Self {
		self.pprof = true;
		self.pprof_sample_rate = default_rate;
		self.pprof_sample_rate_from_env = true;
		self
	}

	/// Enable coarse process-wide allocation and free counters.
	pub const fn with_stats(mut self) -> Self {
		self.stats = true;
		self
	}

	fn effective_pprof_sample_rate(&self) -> usize {
		if self.pprof_sample_rate_from_env {
			let resolved = self.resolved_pprof_sample_rate.load(Ordering::Relaxed);
			if resolved != RESOLVED_SAMPLE_RATE_UNINITIALIZED {
				return resolved;
			}

			let resolved = env_pprof_sample_rate(self.pprof_sample_rate);
			self
				.resolved_pprof_sample_rate
				.store(resolved, Ordering::Relaxed);
			resolved
		} else {
			self.pprof_sample_rate
		}
	}

	fn active_pprof_sample_rate(&self) -> Option<usize> {
		if !self.pprof {
			return None;
		}

		let sample_rate = self.effective_pprof_sample_rate();
		CURRENT_PPROF_SAMPLE_RATE.store(sample_rate, Ordering::Relaxed);
		(sample_rate != 0).then_some(sample_rate)
	}

	fn record_allocation_stats(&self, size: usize) {
		if self.stats {
			if LOCAL_STATS
				.try_with(|stats| stats.record_allocation(size as u64))
				.is_err()
			{
				GLOBAL_STATS
					.allocated
					.fetch_add(size as u64, Ordering::Relaxed);
				GLOBAL_STATS.allocations.fetch_add(1, Ordering::Relaxed);
			}
		}
	}

	fn record_deallocation_stats(&self, size: usize) {
		if self.stats {
			if LOCAL_STATS
				.try_with(|stats| stats.record_deallocation(size as u64))
				.is_err()
			{
				GLOBAL_STATS.freed.fetch_add(size as u64, Ordering::Relaxed);
				GLOBAL_STATS.frees.fetch_add(1, Ordering::Relaxed);
			}
		}
	}

	#[cfg(test)]
	fn record_allocation(&self, ptr: usize, size: usize) {
		self.record_allocation_stats(size);
		if let Some(sample_rate) = self.active_pprof_sample_rate() {
			self.record_profile_allocation(ptr, size, sample_rate);
		}
	}

	fn record_profile_allocation(&self, ptr: usize, size: usize, sample_rate: usize) {
		if should_sample_allocation(size, sample_rate) {
			let trace = HashedBacktrace::capture();
			self.record_allocation_with_trace(ptr, size, trace);
		}
	}

	fn record_tracked_allocation(&self, ptr: *mut u8, size: usize, sample_rate: Option<usize>) {
		if ptr.is_null() {
			return;
		}

		self.record_allocation_stats(size);
		if let Some(sample_rate) = sample_rate {
			self.record_profile_allocation(ptr as usize, size, sample_rate);
		}
	}

	fn tracking_is_recursive(&self, sample_rate: Option<usize>) -> bool {
		(sample_rate.is_some() || self.stats) && IN_ALLOC.try_with(|x| x.get()).unwrap_or(true)
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

	fn take_allocation_record(&self, ptr: usize) -> Option<AllocationRecord> {
		if self.pprof && self.effective_pprof_sample_rate() != 0 {
			POINTER_MAP.remove(&ptr).map(|(_, record)| record)
		} else {
			None
		}
	}

	fn restore_allocation_record(&self, ptr: usize, record: AllocationRecord) {
		POINTER_MAP.insert(ptr, record);
	}

	fn finish_deallocation(&self, record: Option<AllocationRecord>, size: usize) {
		let freed_size = record.as_ref().map(|record| record.size).unwrap_or(size);
		self.record_deallocation_stats(freed_size);

		let Some(record) = record else {
			return;
		};

		let mut stats = TRACE_MAP.entry(record.trace).or_default();
		stats.freed += freed_size as u64;
		stats.frees += 1;
	}

	#[cfg(test)]
	fn record_deallocation(&self, ptr: usize, size: usize) {
		let record = self.take_allocation_record(ptr);
		self.finish_deallocation(record, size);
	}

	#[cfg(test)]
	fn record_reallocation(&self, old_ptr: usize, old_size: usize, new_ptr: usize, new_size: usize) {
		let record = self.take_allocation_record(old_ptr);
		self.finish_deallocation(record, old_size);
		self.record_allocation(new_ptr, new_size);
	}
}

fn enter_alloc<T>(func: impl FnOnce() -> T) -> T {
	let Ok(current_value) = IN_ALLOC.try_with(|x| {
		let current_value = x.get();
		x.set(true);
		current_value
	}) else {
		return func();
	};
	let output = func();
	let _ = IN_ALLOC.try_with(|x| x.set(current_value));
	output
}

thread_local! {
	/// Used to avoid recursive alloc/dealloc calls for interior allocation.
	static IN_ALLOC: Cell<bool> = const { Cell::new(false) };
	static NEXT_SAMPLE: Cell<i64> = const { Cell::new(i64::MIN) };
	static NEXT_SAMPLE_RATE: Cell<usize> = const { Cell::new(usize::MAX) };
	static RNG_STATE: Cell<u64> = const { Cell::new(0) };
	static LOCAL_STATS: LocalAllocationStats = const { LocalAllocationStats::new() };
}

static GLOBAL_STATS: stats::AtomicAllocations = stats::AtomicAllocations::new();
static ENV_PPROF_SAMPLE_RATE_STATE: AtomicU8 = AtomicU8::new(ENV_SAMPLE_RATE_UNINITIALIZED);
static ENV_PPROF_SAMPLE_RATE_VALUE: AtomicUsize = AtomicUsize::new(DEFAULT_PPROF_SAMPLE_RATE);
static CURRENT_PPROF_SAMPLE_RATE: AtomicUsize = AtomicUsize::new(DEFAULT_PPROF_SAMPLE_RATE);

lazy_static::lazy_static! {
	static ref POINTER_MAP: DashMap<usize, AllocationRecord> = DashMap::new();
	static ref TRACE_MAP: DashMap<HashedBacktrace, stats::Allocations> = DashMap::new();
}

/// Return a snapshot of coarse process-wide allocation counters.
///
/// These counters are updated only when the global allocator wrapper was
/// configured with [`PprofAlloc::with_stats`].
pub fn allocation_stats() -> stats::Allocations {
	let _ = LOCAL_STATS.try_with(|stats| stats.flush());
	GLOBAL_STATS.snapshot()
}

/// Return the stack capture mode compiled into this build.
///
/// Linux x86_64/aarch64 builds use the fast frame-pointer unwinder when the
/// default `frame-pointer` feature is enabled. Other builds use the `backtrace`
/// crate fallback.
pub const fn capture_mode() -> CaptureMode {
	trace::capture_mode()
}

fn current_pprof_sample_rate() -> usize {
	CURRENT_PPROF_SAMPLE_RATE.load(Ordering::Relaxed)
}

fn env_pprof_sample_rate(default_rate: usize) -> usize {
	match ENV_PPROF_SAMPLE_RATE_STATE.load(Ordering::Acquire) {
		ENV_SAMPLE_RATE_SET => return ENV_PPROF_SAMPLE_RATE_VALUE.load(Ordering::Relaxed),
		ENV_SAMPLE_RATE_UNSET => return default_rate,
		_ => {},
	}

	if let Some(sample_rate) = read_pprof_sample_rate_env() {
		ENV_PPROF_SAMPLE_RATE_VALUE.store(sample_rate, Ordering::Relaxed);
		ENV_PPROF_SAMPLE_RATE_STATE.store(ENV_SAMPLE_RATE_SET, Ordering::Release);
		sample_rate
	} else {
		ENV_PPROF_SAMPLE_RATE_STATE.store(ENV_SAMPLE_RATE_UNSET, Ordering::Release);
		default_rate
	}
}

fn read_pprof_sample_rate_env() -> Option<usize> {
	let ptr = unsafe { libc::getenv(PPROF_SAMPLE_RATE_ENV_CSTR.as_ptr().cast()) };
	if ptr.is_null() {
		return None;
	}

	let mut value = 0usize;
	let mut cursor = ptr.cast::<u8>();
	let mut saw_digit = false;
	loop {
		let byte = unsafe { *cursor };
		if byte == 0 {
			break;
		}
		if !byte.is_ascii_digit() {
			return None;
		}
		saw_digit = true;
		value = value
			.saturating_mul(10)
			.saturating_add((byte - b'0') as usize);
		cursor = unsafe { cursor.add(1) };
	}

	saw_digit.then_some(value)
}

fn should_sample_allocation(size: usize, sample_rate: usize) -> bool {
	if size == 0 || sample_rate == 0 {
		return false;
	}
	if sample_rate == 1 {
		return true;
	}

	NEXT_SAMPLE
		.try_with(|next_sample| {
			NEXT_SAMPLE_RATE.try_with(|next_sample_rate| {
				if next_sample_rate.get() != sample_rate {
					next_sample.set(next_sample_distance(sample_rate));
					next_sample_rate.set(sample_rate);
				}

				let next = next_sample
					.get()
					.saturating_sub(i64::try_from(size).unwrap_or(i64::MAX));
				if next < 0 {
					next_sample.set(next_sample_distance(sample_rate));
					true
				} else {
					next_sample.set(next);
					false
				}
			})
		})
		.ok()
		.and_then(Result::ok)
		.unwrap_or(false)
}

fn next_sample_distance(sample_rate: usize) -> i64 {
	match sample_rate {
		0 => i64::MAX,
		1 => 0,
		rate => i64::from(fast_exp_rand(rate)),
	}
}

fn fast_exp_rand(mean: usize) -> i32 {
	let mean = mean.min(MAX_FAST_EXP_RAND_MEAN);
	if mean == 0 {
		return 0;
	}

	let q = cheap_random_n(1 << RANDOM_BIT_COUNT) + 1;
	let qlog = ((q as f64).log2() - RANDOM_BIT_COUNT as f64).min(0.0);
	(qlog * (-std::f64::consts::LN_2 * mean as f64)) as i32 + 1
}

fn cheap_random_n(n: u32) -> u32 {
	(cheap_random() % u64::from(n)) as u32
}

fn cheap_random() -> u64 {
	RNG_STATE
		.try_with(|state| {
			let mut x = state.get();
			if x == 0 {
				x = random_seed();
			}
			x ^= x >> 12;
			x ^= x << 25;
			x ^= x >> 27;
			state.set(x);
			x.wrapping_mul(0x2545_f491_4f6c_dd1d)
		})
		.unwrap_or_else(|_| random_seed())
}

fn random_seed() -> u64 {
	let stack_addr = &() as *const () as usize as u64;
	let time = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|duration| duration.as_nanos() as u64)
		.unwrap_or(0);
	let seed = stack_addr ^ time ^ 0x9e37_79b9_7f4a_7c15;
	if seed == 0 {
		0x9e37_79b9_7f4a_7c15
	} else {
		seed
	}
}

/// Capture a best-effort process memory snapshot.
///
/// Individual probes that fail are represented as `None` in the returned
/// [`MemorySnapshot`]. This function intentionally does not fail as a whole.
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
		allocator: allocator::snapshot(),
		cgroup: stats::cgroups::get_stats().ok(),
		smaps: stats::smaps::rollup().ok(),
	})
}

fn pprof_summary() -> PprofSummary {
	let mut summary = PprofSummary::default();
	let sample_rate = current_pprof_sample_rate();
	for entry in TRACE_MAP.iter() {
		let stats = entry.value();
		let values = HeapSampleValues::from_allocations(stats, sample_rate);
		summary.total_stacks += 1;
		summary.alloc_space_bytes += values.alloc_space.max(0) as u64;
		summary.inuse_space_bytes += values.inuse_space.max(0) as u64;
		summary.alloc_objects += values.alloc_objects.max(0) as u64;
		summary.inuse_objects += values.inuse_objects.max(0) as u64;
		if values.inuse_space > 0 {
			summary.live_stacks += 1;
		}
	}
	summary
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for PprofAlloc<A> {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		unsafe {
			let sample_rate = self.active_pprof_sample_rate();
			if self.tracking_is_recursive(sample_rate) {
				return self.inner.alloc(layout);
			}

			if sample_rate.is_none() {
				let ptr = self.inner.alloc(layout);
				self.record_tracked_allocation(ptr, layout.size(), sample_rate);
				return ptr;
			}

			enter_alloc(|| {
				let ptr = self.inner.alloc(layout);
				self.record_tracked_allocation(ptr, layout.size(), sample_rate);
				ptr
			})
		}
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		unsafe {
			let sample_rate = self.active_pprof_sample_rate();
			if self.tracking_is_recursive(sample_rate) {
				return self.inner.alloc_zeroed(layout);
			}

			if sample_rate.is_none() {
				let ptr = self.inner.alloc_zeroed(layout);
				self.record_tracked_allocation(ptr, layout.size(), sample_rate);
				return ptr;
			}

			enter_alloc(|| {
				let ptr = self.inner.alloc_zeroed(layout);
				self.record_tracked_allocation(ptr, layout.size(), sample_rate);
				ptr
			})
		}
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe {
			let sample_rate = self.active_pprof_sample_rate();
			if self.tracking_is_recursive(sample_rate) {
				self.inner.dealloc(ptr, layout);
				return;
			}

			if sample_rate.is_none() {
				self.inner.dealloc(ptr, layout);
				self.record_deallocation_stats(layout.size());
				return;
			}

			enter_alloc(|| {
				let record = self.take_allocation_record(ptr as usize);
				self.inner.dealloc(ptr, layout);
				self.finish_deallocation(record, layout.size());
			});
		}
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		unsafe {
			let sample_rate = self.active_pprof_sample_rate();
			if self.tracking_is_recursive(sample_rate) {
				return self.inner.realloc(ptr, layout, new_size);
			}

			if sample_rate.is_none() {
				let new_ptr = self.inner.realloc(ptr, layout, new_size);
				if !new_ptr.is_null() {
					self.record_deallocation_stats(layout.size());
					self.record_allocation_stats(new_size);
				}
				return new_ptr;
			}

			enter_alloc(|| {
				let record = self.take_allocation_record(ptr as usize);
				let new_ptr = self.inner.realloc(ptr, layout, new_size);
				if !new_ptr.is_null() {
					self.finish_deallocation(record, layout.size());
					self.record_tracked_allocation(new_ptr, new_size, sample_rate);
				} else if let Some(record) = record {
					self.restore_allocation_record(ptr as usize, record);
				}
				new_ptr
			})
		}
	}
}

pub fn generate_pprof() -> anyhow::Result<Vec<u8>> {
	enter_alloc(|| {
		let sample_rate = current_pprof_sample_rate();
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
			let sample_values = HeapSampleValues::from_allocations(entry.value(), sample_rate);
			if sample_values.alloc_space == 0
				&& sample_values.inuse_space == 0
				&& sample_values.alloc_objects == 0
				&& sample_values.inuse_objects == 0
			{
				continue;
			}

			profile.push_stack(
				WeightedStack {
					addrs: entry.key().addrs(),
					values: smallvec::smallvec![
						sample_values.alloc_objects,
						sample_values.alloc_space,
						sample_values.inuse_objects,
						sample_values.inuse_space
					],
				},
				None,
			);
		}

		Ok(profile.to_pprof_with_period(
			&[
				("alloc_objects", "count"),
				("alloc_space", "bytes"),
				("inuse_objects", "count"),
				("inuse_space", "bytes"),
			],
			("space", "bytes"),
			sample_rate as i64,
			None,
		))
	})
}

#[doc(hidden)]
#[macro_export]
macro_rules! __pprof_alloc_register_allocator_kind {
	($kind:expr) => {
		const _: () = {
			#[cfg(target_os = "linux")]
			#[used]
			#[unsafe(link_section = ".init_array")]
			static INIT_ARRAY: extern "C" fn() = {
				extern "C" fn init() {
					$crate::allocator::configure($kind);
				}
				init
			};
		};
	};
}

#[macro_export]
macro_rules! declare_allocator_kind {
	($kind:expr $(;)?) => {
		$crate::__pprof_alloc_register_allocator_kind!($kind);
	};
}

#[cfg(test)]
fn reset_tracking_state() {
	POINTER_MAP.clear();
	TRACE_MAP.clear();
	GLOBAL_STATS.allocated.store(0, Ordering::Relaxed);
	GLOBAL_STATS.freed.store(0, Ordering::Relaxed);
	GLOBAL_STATS.allocations.store(0, Ordering::Relaxed);
	GLOBAL_STATS.frees.store(0, Ordering::Relaxed);
	let _ = LOCAL_STATS.try_with(|stats| stats.reset());
	CURRENT_PPROF_SAMPLE_RATE.store(1, Ordering::Relaxed);
	ENV_PPROF_SAMPLE_RATE_STATE.store(ENV_SAMPLE_RATE_UNINITIALIZED, Ordering::Relaxed);
	ENV_PPROF_SAMPLE_RATE_VALUE.store(DEFAULT_PPROF_SAMPLE_RATE, Ordering::Relaxed);
	let _ = NEXT_SAMPLE.try_with(|next_sample| next_sample.set(i64::MIN));
	let _ = NEXT_SAMPLE_RATE.try_with(|next_sample_rate| next_sample_rate.set(usize::MAX));
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
	fn sample_rate_one_records_every_profile_allocation() {
		let _guard = TEST_GUARD.lock();
		reset_tracking_state();

		let alloc = PprofAlloc::new().with_pprof_sample_rate(1);
		alloc.record_allocation(0x1000, 128);
		alloc.record_allocation(0x2000, 64);

		assert_eq!(current_pprof_sample_rate(), 1);
		assert_eq!(POINTER_MAP.len(), 2);
		assert_eq!(pprof_summary().alloc_space_bytes, 192);
		assert_eq!(pprof_summary().alloc_objects, 2);
	}

	#[test]
	fn sample_rate_zero_disables_profile_allocation_records() {
		let _guard = TEST_GUARD.lock();
		reset_tracking_state();

		let alloc = PprofAlloc::new().with_pprof_sample_rate(0).with_stats();
		alloc.record_allocation(0x1000, 128);
		alloc.record_deallocation(0x1000, 128);

		assert_eq!(current_pprof_sample_rate(), 0);
		assert!(POINTER_MAP.is_empty());
		assert!(TRACE_MAP.is_empty());
		assert_eq!(allocation_stats().allocated, 128);
		assert_eq!(allocation_stats().freed, 128);
	}

	#[test]
	fn sampled_heap_values_are_scaled_to_estimates() {
		let (count, size) = scale_heap_sample(1, 1024, 512);

		assert_eq!(count, 1);
		assert!((1180..=1190).contains(&size));
	}

	#[test]
	fn env_sample_rate_is_read_lazily() {
		let _guard = TEST_GUARD.lock();
		reset_tracking_state();

		unsafe {
			std::env::set_var(PPROF_SAMPLE_RATE_ENV, "1");
		}
		let alloc = PprofAlloc::new().with_pprof_sample_rate_from_env(DEFAULT_PPROF_SAMPLE_RATE);
		alloc.record_allocation(0x1000, 128);
		unsafe {
			std::env::remove_var(PPROF_SAMPLE_RATE_ENV);
		}

		assert_eq!(current_pprof_sample_rate(), 1);
		assert_eq!(POINTER_MAP.len(), 1);
	}

	#[test]
	fn env_sample_rate_uses_configured_default_when_unset() {
		let _guard = TEST_GUARD.lock();
		reset_tracking_state();

		unsafe {
			std::env::remove_var(PPROF_SAMPLE_RATE_ENV);
		}
		let alloc = PprofAlloc::new().with_pprof_sample_rate_from_env(1);
		alloc.record_allocation(0x1000, 128);

		assert_eq!(current_pprof_sample_rate(), 1);
		assert_eq!(POINTER_MAP.len(), 1);
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

		let alloc = PprofAlloc::new().with_pprof_sample_rate(1);
		alloc.record_allocation_with_trace(0x3000, 32, HashedBacktrace::capture());
		alloc.record_reallocation(0x3000, 32, 0x4000, 96);

		let total_live_bytes: u64 = TRACE_MAP
			.iter()
			.map(|entry| entry.value().in_use_bytes())
			.sum();
		assert_eq!(total_live_bytes, 96);
		assert_eq!(POINTER_MAP.get(&0x4000).unwrap().size, 96);
		assert!(!POINTER_MAP.contains_key(&0x3000));
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
		assert_eq!(snapshot.capture_mode, capture_mode());
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
