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

pub const DEFAULT_PPROF_SAMPLE_RATE: usize = 512 * 1024;
pub const PPROF_SAMPLE_RATE_ENV: &str = "PPROF_ALLOC_SAMPLE_RATE";

const PPROF_SAMPLE_RATE_ENV_CSTR: &[u8] = b"PPROF_ALLOC_SAMPLE_RATE\0";
const MAX_FAST_EXP_RAND_MEAN: usize = 0x7000000;
const RANDOM_BIT_COUNT: u32 = 26;
const ENV_SAMPLE_RATE_UNINITIALIZED: u8 = 0;
const ENV_SAMPLE_RATE_SET: u8 = 1;
const ENV_SAMPLE_RATE_UNSET: u8 = 2;

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

#[derive(Clone, Debug, Serialize)]
pub struct Probe<T> {
	pub value: Option<T>,
	pub error: Option<String>,
}

impl<T> Probe<T> {
	pub(crate) fn ok(value: T) -> Self {
		Self {
			value: Some(value),
			error: None,
		}
	}

	pub(crate) fn err(error: impl ToString) -> Self {
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
	pub allocator: allocator::AllocatorSnapshot,
	pub cgroup: Probe<stats::cgroups::MemoryStat>,
	pub smaps: Probe<stats::smaps::ProcessStats>,
}

impl Default for PprofAlloc<System> {
	fn default() -> Self {
		Self::new()
	}
}

impl PprofAlloc<System> {
	pub const fn new() -> Self {
		Self::from_allocator(System)
	}
}

impl<A> PprofAlloc<A> {
	pub const fn from_allocator(inner: A) -> Self {
		PprofAlloc {
			inner,
			pprof: false,
			stats: false,
			pprof_sample_rate: DEFAULT_PPROF_SAMPLE_RATE,
			pprof_sample_rate_from_env: false,
		}
	}

	pub const fn with_pprof(mut self) -> Self {
		self.pprof = true;
		self
	}

	pub const fn with_pprof_sample_rate(mut self, bytes: usize) -> Self {
		self.pprof = true;
		self.pprof_sample_rate = bytes;
		self.pprof_sample_rate_from_env = false;
		self
	}

	pub const fn with_pprof_sample_rate_from_env(mut self, default_rate: usize) -> Self {
		self.pprof = true;
		self.pprof_sample_rate = default_rate;
		self.pprof_sample_rate_from_env = true;
		self
	}

	pub const fn with_stats(mut self) -> Self {
		self.stats = true;
		self
	}

	fn effective_pprof_sample_rate(&self) -> usize {
		if self.pprof_sample_rate_from_env {
			env_pprof_sample_rate(self.pprof_sample_rate)
		} else {
			self.pprof_sample_rate
		}
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

		let sample_rate = self.effective_pprof_sample_rate();
		CURRENT_PPROF_SAMPLE_RATE.store(sample_rate, Ordering::Relaxed);
		if !should_sample_allocation(size, sample_rate) {
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
		let record = if self.pprof && self.effective_pprof_sample_rate() != 0 {
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
	static NEXT_SAMPLE: Cell<i64> = const { Cell::new(i64::MIN) };
	static NEXT_SAMPLE_RATE: Cell<usize> = const { Cell::new(usize::MAX) };
	static RNG_STATE: Cell<u64> = const { Cell::new(0) };
}

static GLOBAL_STATS: stats::AtomicAllocations = stats::AtomicAllocations::new();
static ENV_PPROF_SAMPLE_RATE_STATE: AtomicU8 = AtomicU8::new(ENV_SAMPLE_RATE_UNINITIALIZED);
static ENV_PPROF_SAMPLE_RATE_VALUE: AtomicUsize = AtomicUsize::new(DEFAULT_PPROF_SAMPLE_RATE);
static CURRENT_PPROF_SAMPLE_RATE: AtomicUsize = AtomicUsize::new(DEFAULT_PPROF_SAMPLE_RATE);

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

pub fn current_pprof_sample_rate() -> usize {
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

	NEXT_SAMPLE.with(|next_sample| {
		NEXT_SAMPLE_RATE.with(|next_sample_rate| {
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
	RNG_STATE.with(|state| {
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
		cgroup: Probe::from(stats::cgroups::get_stats()),
		smaps: Probe::from(stats::smaps::rollup()),
	})
}

pub fn collect() -> anyhow::Result<()> {
	enter_alloc(allocator::collect)
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
	CURRENT_PPROF_SAMPLE_RATE.store(1, Ordering::Relaxed);
	ENV_PPROF_SAMPLE_RATE_STATE.store(ENV_SAMPLE_RATE_UNINITIALIZED, Ordering::Relaxed);
	ENV_PPROF_SAMPLE_RATE_VALUE.store(DEFAULT_PPROF_SAMPLE_RATE, Ordering::Relaxed);
	NEXT_SAMPLE.with(|next_sample| next_sample.set(i64::MIN));
	NEXT_SAMPLE_RATE.with(|next_sample_rate| next_sample_rate.set(usize::MAX));
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
