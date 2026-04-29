use crate::Probe;
use crate::stats;
use anyhow::Result;
use prometheus_client::collector::Collector;
use prometheus_client::encoding::DescriptorEncoder;
use prometheus_client::encoding::EncodeMetric;
use prometheus_client::metrics::gauge::ConstGauge;
use prometheus_client::metrics::info::Info;
use serde::Serialize;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocatorKind {
	Undeclared,
	Glibc,
	Jemalloc,
	Mimalloc,
}

#[derive(Clone, Debug, Serialize)]
pub struct AllocatorSnapshot {
	pub kind: AllocatorKind,
	pub comparable: Probe<AllocatorComparisonStats>,
	pub specific: Probe<AllocatorSpecificDetails>,
}

#[derive(Debug, Clone)]
pub struct PrometheusCollector {}

impl PrometheusCollector {
	pub fn register(registry: &mut prometheus_client::registry::Registry) {
		registry.register_collector(Box::new(Self {}))
	}
}

impl Collector for PrometheusCollector {
	fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
		let snapshot = snapshot();
		let info_metric = Info::new(vec![("allocator", snapshot.kind.as_str())]);
		let info_encoder = encoder.encode_descriptor(
			"allocator_info",
			"allocator identity for this process",
			None,
			info_metric.metric_type(),
		)?;
		info_metric.encode(info_encoder)?;
		let configured_metric = ConstGauge::new(u64::from(snapshot.kind != AllocatorKind::Undeclared));
		let configured_encoder = encoder.encode_descriptor(
			"allocator_configured",
			"whether allocator kind was explicitly declared for this process",
			None,
			configured_metric.metric_type(),
		)?;
		configured_metric.encode(configured_encoder)?;

		let mut encode = |value: Option<u64>, name: &'static str, help: &str| {
			let Some(value) = value else {
				return Ok(());
			};
			let metric = ConstGauge::new(value);
			let metric_encoder = encoder.encode_descriptor(name, help, None, metric.metric_type())?;
			metric.encode(metric_encoder)?;
			Ok(())
		};

		let Some(comparable) = snapshot.comparable.value.as_ref() else {
			return Ok(());
		};

		encode(
			comparable.allocated_bytes,
			"allocator_allocated_bytes",
			"bytes allocated according to the current allocator",
		)?;
		encode(
			comparable.active_bytes,
			"allocator_active_bytes",
			"bytes currently active according to the current allocator",
		)?;
		encode(
			comparable.resident_bytes,
			"allocator_resident_bytes",
			"resident bytes attributed to the current allocator",
		)?;
		encode(
			comparable.mapped_bytes,
			"allocator_mapped_bytes",
			"bytes mapped or reserved by the current allocator",
		)?;
		encode(
			comparable.retained_bytes,
			"allocator_retained_bytes",
			"bytes retained but not currently active according to the current allocator",
		)?;
		encode(
			comparable.metadata_bytes,
			"allocator_metadata_bytes",
			"bytes used for allocator metadata",
		)?;
		encode(
			comparable.committed_bytes,
			"allocator_committed_bytes",
			"bytes committed by the current allocator",
		)?;
		encode(
			comparable.allocator_structures,
			"allocator_structures",
			"allocator structures such as heaps or arenas",
		)?;
		Ok(())
	}
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AllocatorComparisonStats {
	pub allocated_bytes: Option<u64>,
	pub active_bytes: Option<u64>,
	pub resident_bytes: Option<u64>,
	pub mapped_bytes: Option<u64>,
	pub retained_bytes: Option<u64>,
	pub metadata_bytes: Option<u64>,
	pub committed_bytes: Option<u64>,
	pub allocator_structures: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AllocatorSpecificDetails {
	Glibc(GlibcStats),
	#[cfg(feature = "allocator-jemalloc")]
	Jemalloc(JemallocStats),
	#[cfg(feature = "allocator-mimalloc")]
	Mimalloc(MimallocStats),
}

#[derive(Clone, Debug, Serialize)]
pub struct GlibcStats {
	pub system_max: u64,
	pub system_current: u64,
	pub free_bytes: u64,
	pub mmap_current: u64,
	pub in_use_bytes: u64,
	pub heaps: u64,
}

impl From<&stats::malloc::MallocInfo> for GlibcStats {
	fn from(info: &stats::malloc::MallocInfo) -> Self {
		Self {
			system_max: info.system_max(),
			system_current: info.system_current(),
			free_bytes: info.free_bytes(),
			mmap_current: info.mmap_bytes(),
			in_use_bytes: info.in_use_bytes(),
			heaps: info.heaps(),
		}
	}
}

impl From<&GlibcStats> for AllocatorComparisonStats {
	fn from(stats: &GlibcStats) -> Self {
		Self {
			allocated_bytes: Some(stats.in_use_bytes),
			active_bytes: None,
			resident_bytes: None,
			mapped_bytes: Some(stats.system_current.saturating_add(stats.mmap_current)),
			retained_bytes: Some(stats.free_bytes),
			metadata_bytes: None,
			committed_bytes: None,
			allocator_structures: Some(stats.heaps),
		}
	}
}

#[cfg(feature = "allocator-jemalloc")]
#[derive(Clone, Debug, Serialize)]
pub struct JemallocStats {
	pub allocated: u64,
	pub active: u64,
	pub metadata: u64,
	pub resident: u64,
	pub mapped: u64,
	pub retained: u64,
	pub background_thread: bool,
}

#[cfg(feature = "allocator-jemalloc")]
impl From<&JemallocStats> for AllocatorComparisonStats {
	fn from(stats: &JemallocStats) -> Self {
		Self {
			allocated_bytes: Some(stats.allocated),
			active_bytes: Some(stats.active),
			resident_bytes: Some(stats.resident),
			mapped_bytes: Some(stats.mapped),
			retained_bytes: Some(stats.retained),
			metadata_bytes: Some(stats.metadata),
			committed_bytes: None,
			allocator_structures: None,
		}
	}
}

#[cfg(feature = "allocator-mimalloc")]
#[derive(Clone, Debug, Serialize)]
pub struct MimallocStats {
	pub version: u32,
	pub allocated_current: u64,
	pub allocated_peak: u64,
	pub reserved_current: u64,
	pub reserved_peak: u64,
	pub committed_current: u64,
	pub committed_peak: u64,
	pub reset_current: u64,
	pub purged_current: u64,
	pub page_committed_current: u64,
	pub pages_current: u64,
	pub pages_abandoned_current: u64,
	pub segments_current: u64,
	pub segments_abandoned_current: u64,
	pub threads_current: u64,
	pub requested_current: u64,
	pub requested_peak: u64,
	pub process_rss_current: u64,
	pub process_rss_peak: u64,
	pub process_commit_current: u64,
	pub process_commit_peak: u64,
	pub page_faults: u64,
	pub arenas: u64,
}

#[cfg(feature = "allocator-mimalloc")]
impl From<&MimallocStats> for AllocatorComparisonStats {
	fn from(stats: &MimallocStats) -> Self {
		Self {
			allocated_bytes: Some(stats.allocated_current),
			active_bytes: None,
			resident_bytes: Some(stats.process_rss_current),
			mapped_bytes: Some(stats.reserved_current),
			retained_bytes: None,
			metadata_bytes: None,
			committed_bytes: Some(stats.committed_current),
			allocator_structures: Some(stats.arenas),
		}
	}
}

#[cfg(feature = "allocator-mimalloc")]
#[repr(C)]
struct MiStatCount {
	total: i64,
	peak: i64,
	current: i64,
}

#[cfg(feature = "allocator-mimalloc")]
#[repr(C)]
struct MiStatCounter {
	total: i64,
}

#[cfg(feature = "allocator-mimalloc")]
#[repr(C)]
struct MiStats {
	version: i32,
	pages: MiStatCount,
	reserved: MiStatCount,
	committed: MiStatCount,
	reset: MiStatCount,
	purged: MiStatCount,
	page_committed: MiStatCount,
	pages_abandoned: MiStatCount,
	threads: MiStatCount,
	malloc_normal: MiStatCount,
	malloc_huge: MiStatCount,
	malloc_requested: MiStatCount,
	mmap_calls: MiStatCounter,
	commit_calls: MiStatCounter,
	reset_calls: MiStatCounter,
	purge_calls: MiStatCounter,
	arena_count: MiStatCounter,
	malloc_normal_count: MiStatCounter,
	malloc_huge_count: MiStatCounter,
	malloc_guarded_count: MiStatCounter,
	arena_rollback_count: MiStatCounter,
	arena_purges: MiStatCounter,
	pages_extended: MiStatCounter,
	pages_retire: MiStatCounter,
	page_searches: MiStatCounter,
	segments: MiStatCount,
	segments_abandoned: MiStatCount,
	segments_cache: MiStatCount,
	segments_reserved: MiStatCount,
	pages_reclaim_on_alloc: MiStatCounter,
	pages_reclaim_on_free: MiStatCounter,
	pages_reabandon_full: MiStatCounter,
	pages_unabandon_busy_wait: MiStatCounter,
	stat_reserved: [MiStatCount; 4],
	stat_counter_reserved: [MiStatCounter; 4],
	malloc_bins: [MiStatCount; 74],
	page_bins: [MiStatCount; 74],
}

static CONFIGURED_ALLOCATOR: AtomicU8 = AtomicU8::new(AllocatorKind::Undeclared.as_u8());

impl AllocatorKind {
	const fn as_u8(self) -> u8 {
		match self {
			Self::Undeclared => 0,
			Self::Glibc => 1,
			Self::Jemalloc => 2,
			Self::Mimalloc => 3,
		}
	}

	const fn from_u8(value: u8) -> Self {
		match value {
			1 => Self::Glibc,
			2 => Self::Jemalloc,
			3 => Self::Mimalloc,
			_ => Self::Undeclared,
		}
	}

	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Undeclared => "undeclared",
			Self::Glibc => "glibc",
			Self::Jemalloc => "jemalloc",
			Self::Mimalloc => "mimalloc",
		}
	}
}

pub fn configure(kind: AllocatorKind) {
	CONFIGURED_ALLOCATOR.store(kind.as_u8(), Ordering::Release);
}

pub fn configured() -> AllocatorKind {
	AllocatorKind::from_u8(CONFIGURED_ALLOCATOR.load(Ordering::Acquire))
}

pub fn collect() -> Result<()> {
	collect_for(configured())
}

pub fn collect_for(kind: AllocatorKind) -> Result<()> {
	match kind {
		AllocatorKind::Undeclared => anyhow::bail!(
			"allocator kind is undeclared; add declare_allocator_kind!(...) next to #[global_allocator]"
		),
		AllocatorKind::Glibc => glibc_collect(),
		AllocatorKind::Jemalloc => jemalloc_collect(),
		AllocatorKind::Mimalloc => mimalloc_collect(true),
	}
}

pub fn snapshot() -> AllocatorSnapshot {
	snapshot_for(configured())
}

pub fn snapshot_for(kind: AllocatorKind) -> AllocatorSnapshot {
	match kind {
		AllocatorKind::Undeclared => backend_snapshot(
			kind,
			Err(anyhow::anyhow!(
				"allocator kind is undeclared; add declare_allocator_kind!(...) next to #[global_allocator]"
			)),
		),
		AllocatorKind::Glibc => {
			let result = glibc_snapshot().map(|stats| {
				(
					AllocatorComparisonStats::from(&stats),
					AllocatorSpecificDetails::Glibc(stats),
				)
			});
			backend_snapshot(kind, result)
		},
		AllocatorKind::Jemalloc => backend_snapshot(kind, jemalloc_snapshot_pair()),
		AllocatorKind::Mimalloc => backend_snapshot(kind, mimalloc_snapshot_pair()),
	}
}

fn backend_snapshot(
	kind: AllocatorKind,
	result: Result<(AllocatorComparisonStats, AllocatorSpecificDetails)>,
) -> AllocatorSnapshot {
	match result {
		Ok((comparable, specific)) => AllocatorSnapshot {
			kind,
			comparable: Probe::ok(comparable),
			specific: Probe::ok(specific),
		},
		Err(error) => {
			let message = error.to_string();
			AllocatorSnapshot {
				kind,
				comparable: Probe::err(&message),
				specific: Probe::err(message),
			}
		},
	}
}

fn glibc_snapshot() -> Result<GlibcStats> {
	stats::malloc::info()
		.map(|info| GlibcStats::from(&info))
		.map_err(anyhow::Error::from)
}

fn glibc_collect() -> Result<()> {
	#[cfg(all(target_os = "linux", target_env = "gnu"))]
	{
		stats::malloc::malloc_trim();
		Ok(())
	}

	#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
	{
		anyhow::bail!("glibc collection is only supported on linux-gnu targets")
	}
}

#[cfg(feature = "allocator-jemalloc")]
fn jemalloc_snapshot_pair() -> Result<(AllocatorComparisonStats, AllocatorSpecificDetails)> {
	let stats = jemalloc_snapshot()?;
	Ok((
		AllocatorComparisonStats::from(&stats),
		AllocatorSpecificDetails::Jemalloc(stats),
	))
}

#[cfg(not(feature = "allocator-jemalloc"))]
fn jemalloc_snapshot_pair() -> Result<(AllocatorComparisonStats, AllocatorSpecificDetails)> {
	anyhow::bail!("jemalloc support is not compiled in; enable the `allocator-jemalloc` feature")
}

#[cfg(feature = "allocator-jemalloc")]
fn jemalloc_collect() -> Result<()> {
	use tikv_jemalloc_ctl::{arenas, raw};

	unsafe {
		// Cached thread-local objects need to be released before arena purge has
		// a chance to return pages to the OS.
		let _ = raw::write::<()>(b"thread.tcache.flush\0", ());
	}

	let narenas = arenas::narenas::read().map_err(anyhow::Error::from)?;
	let mut purge_mib = [0; 3];
	raw::name_to_mib(b"arena.0.purge\0", &mut purge_mib).map_err(anyhow::Error::from)?;
	for arena in 0..narenas {
		purge_mib[1] = arena as usize;
		unsafe {
			raw::write_mib::<()>(&purge_mib, ()).map_err(anyhow::Error::from)?;
		}
	}
	Ok(())
}

#[cfg(not(feature = "allocator-jemalloc"))]
fn jemalloc_collect() -> Result<()> {
	anyhow::bail!("jemalloc support is not compiled in; enable the `allocator-jemalloc` feature")
}

#[cfg(feature = "allocator-jemalloc")]
fn jemalloc_snapshot() -> Result<JemallocStats> {
	use tikv_jemalloc_ctl::{background_thread, epoch, stats};

	epoch::advance().map_err(anyhow::Error::from)?;

	Ok(JemallocStats {
		allocated: stats::allocated::read().map_err(anyhow::Error::from)? as u64,
		active: stats::active::read().map_err(anyhow::Error::from)? as u64,
		metadata: stats::metadata::read().map_err(anyhow::Error::from)? as u64,
		resident: stats::resident::read().map_err(anyhow::Error::from)? as u64,
		mapped: stats::mapped::read().map_err(anyhow::Error::from)? as u64,
		retained: stats::retained::read().map_err(anyhow::Error::from)? as u64,
		background_thread: background_thread::read().unwrap_or(false),
	})
}

#[cfg(feature = "allocator-mimalloc")]
fn mimalloc_snapshot_pair() -> Result<(AllocatorComparisonStats, AllocatorSpecificDetails)> {
	let stats = mimalloc_snapshot()?;
	Ok((
		AllocatorComparisonStats::from(&stats),
		AllocatorSpecificDetails::Mimalloc(stats),
	))
}

#[cfg(not(feature = "allocator-mimalloc"))]
fn mimalloc_snapshot_pair() -> Result<(AllocatorComparisonStats, AllocatorSpecificDetails)> {
	anyhow::bail!("mimalloc support is not compiled in; enable the `allocator-mimalloc` feature")
}

#[cfg(feature = "allocator-mimalloc")]
pub fn mimalloc_collect(force: bool) -> Result<()> {
	unsafe {
		libmimalloc_sys::mi_collect(force);
	}
	Ok(())
}

#[cfg(not(feature = "allocator-mimalloc"))]
pub fn mimalloc_collect(_force: bool) -> Result<()> {
	anyhow::bail!("mimalloc support is not compiled in; enable the `allocator-mimalloc` feature")
}

#[cfg(feature = "allocator-mimalloc")]
fn mimalloc_snapshot() -> Result<MimallocStats> {
	use libmimalloc_sys::{mi_process_info, mi_stats_merge};

	unsafe extern "C" {
		// `libmimalloc-sys` exposes most of the extended API we use, but it does
		// not currently bind `mi_stats_get`.
		fn mi_stats_get(stats_size: usize, stats: *mut MiStats);
	}

	let mut stats = MiStats {
		version: 0,
		pages: zero_count(),
		reserved: zero_count(),
		committed: zero_count(),
		reset: zero_count(),
		purged: zero_count(),
		page_committed: zero_count(),
		pages_abandoned: zero_count(),
		threads: zero_count(),
		malloc_normal: zero_count(),
		malloc_huge: zero_count(),
		malloc_requested: zero_count(),
		mmap_calls: zero_counter(),
		commit_calls: zero_counter(),
		reset_calls: zero_counter(),
		purge_calls: zero_counter(),
		arena_count: zero_counter(),
		malloc_normal_count: zero_counter(),
		malloc_huge_count: zero_counter(),
		malloc_guarded_count: zero_counter(),
		arena_rollback_count: zero_counter(),
		arena_purges: zero_counter(),
		pages_extended: zero_counter(),
		pages_retire: zero_counter(),
		page_searches: zero_counter(),
		segments: zero_count(),
		segments_abandoned: zero_count(),
		segments_cache: zero_count(),
		segments_reserved: zero_count(),
		pages_reclaim_on_alloc: zero_counter(),
		pages_reclaim_on_free: zero_counter(),
		pages_reabandon_full: zero_counter(),
		pages_unabandon_busy_wait: zero_counter(),
		stat_reserved: [zero_count(), zero_count(), zero_count(), zero_count()],
		stat_counter_reserved: [
			zero_counter(),
			zero_counter(),
			zero_counter(),
			zero_counter(),
		],
		malloc_bins: std::array::from_fn(|_| zero_count()),
		page_bins: std::array::from_fn(|_| zero_count()),
	};

	let mut elapsed_msecs = 0usize;
	let mut user_msecs = 0usize;
	let mut system_msecs = 0usize;
	let mut current_rss = 0usize;
	let mut peak_rss = 0usize;
	let mut current_commit = 0usize;
	let mut peak_commit = 0usize;
	let mut page_faults = 0usize;

	unsafe {
		// `mi_stats_get` copies the subprocess aggregate only; merge current
		// thread-local stats first so live allocation counters are up to date.
		mi_stats_merge();
		mi_stats_get(std::mem::size_of::<MiStats>(), &mut stats);
		mi_process_info(
			&mut elapsed_msecs,
			&mut user_msecs,
			&mut system_msecs,
			&mut current_rss,
			&mut peak_rss,
			&mut current_commit,
			&mut peak_commit,
			&mut page_faults,
		);
	}

	let _ = (elapsed_msecs, user_msecs, system_msecs);

	if stats.version == 0 {
		anyhow::bail!("mimalloc statistics are unavailable");
	}

	Ok(MimallocStats {
		version: stats.version as u32,
		allocated_current: stats
			.malloc_normal
			.current
			.max(0)
			.saturating_add(stats.malloc_huge.current.max(0)) as u64,
		allocated_peak: stats
			.malloc_normal
			.peak
			.max(0)
			.saturating_add(stats.malloc_huge.peak.max(0)) as u64,
		reserved_current: stats.reserved.current.max(0) as u64,
		reserved_peak: stats.reserved.peak.max(0) as u64,
		committed_current: stats.committed.current.max(0) as u64,
		committed_peak: stats.committed.peak.max(0) as u64,
		reset_current: stats.reset.current.max(0) as u64,
		purged_current: stats.purged.current.max(0) as u64,
		page_committed_current: stats.page_committed.current.max(0) as u64,
		pages_current: stats.pages.current.max(0) as u64,
		pages_abandoned_current: stats.pages_abandoned.current.max(0) as u64,
		segments_current: stats.segments.current.max(0) as u64,
		segments_abandoned_current: stats.segments_abandoned.current.max(0) as u64,
		threads_current: stats.threads.current.max(0) as u64,
		requested_current: stats.malloc_requested.current.max(0) as u64,
		requested_peak: stats.malloc_requested.peak.max(0) as u64,
		process_rss_current: current_rss as u64,
		process_rss_peak: peak_rss as u64,
		process_commit_current: current_commit as u64,
		process_commit_peak: peak_commit as u64,
		page_faults: page_faults as u64,
		arenas: stats.arena_count.total.max(0) as u64,
	})
}

#[cfg(feature = "allocator-mimalloc")]
const fn zero_count() -> MiStatCount {
	MiStatCount {
		total: 0,
		peak: 0,
		current: 0,
	}
}

#[cfg(feature = "allocator-mimalloc")]
const fn zero_counter() -> MiStatCounter {
	MiStatCounter { total: 0 }
}

#[cfg(test)]
mod tests {
	use super::*;
	use parking_lot::Mutex;
	use prometheus_client::encoding::text::encode;
	use prometheus_client::registry::Registry;

	static TEST_GUARD: Mutex<()> = Mutex::new(());

	#[test]
	fn snapshot_reports_undeclared_allocator_as_error() {
		let _guard = TEST_GUARD.lock();
		configure(AllocatorKind::Undeclared);

		let snapshot = snapshot();
		assert_eq!(snapshot.kind, AllocatorKind::Undeclared);
		assert!(snapshot.comparable.value.is_none());
		assert!(snapshot.specific.value.is_none());
		assert_eq!(
			snapshot.comparable.error.as_deref(),
			Some(
				"allocator kind is undeclared; add declare_allocator_kind!(...) next to #[global_allocator]"
			)
		);
	}

	#[test]
	fn collect_reports_undeclared_allocator_as_error() {
		let _guard = TEST_GUARD.lock();
		configure(AllocatorKind::Undeclared);

		let err = collect().expect_err("undeclared allocator should fail collect");
		assert_eq!(
			err.to_string(),
			"allocator kind is undeclared; add declare_allocator_kind!(...) next to #[global_allocator]"
		);
	}

	#[test]
	fn prometheus_collector_emits_allocator_info_metric() {
		let _guard = TEST_GUARD.lock();
		configure(AllocatorKind::Glibc);

		let mut registry = Registry::default();
		PrometheusCollector::register(&mut registry);

		let mut output = String::new();
		encode(&mut output, &registry).expect("allocator metrics should encode");

		assert!(output.contains("# TYPE allocator_info info"));
		assert!(output.contains("allocator_info_info{allocator=\"glibc\"} 1"));
		assert!(output.contains("allocator_configured 1"));
	}

	#[test]
	fn prometheus_collector_emits_undeclared_allocator_signal() {
		let _guard = TEST_GUARD.lock();
		configure(AllocatorKind::Undeclared);

		let mut registry = Registry::default();
		PrometheusCollector::register(&mut registry);

		let mut output = String::new();
		encode(&mut output, &registry).expect("allocator metrics should encode");

		assert!(output.contains("allocator_info_info{allocator=\"undeclared\"} 1"));
		assert!(output.contains("allocator_configured 0"));
	}

	#[test]
	fn glibc_comparison_stats_use_in_use_and_free_bytes() {
		let comparable = AllocatorComparisonStats::from(&GlibcStats {
			system_max: 8192,
			system_current: 4096,
			free_bytes: 1024,
			mmap_current: 512,
			in_use_bytes: 3584,
			heaps: 3,
		});

		assert_eq!(comparable.allocated_bytes, Some(3584));
		assert_eq!(comparable.mapped_bytes, Some(4608));
		assert_eq!(comparable.retained_bytes, Some(1024));
		assert_eq!(comparable.allocator_structures, Some(3));
	}

	#[cfg(all(target_os = "linux", target_env = "gnu"))]
	#[test]
	fn glibc_collect_succeeds() {
		let _guard = TEST_GUARD.lock();
		collect_for(AllocatorKind::Glibc).expect("glibc collection should succeed");
	}

	#[cfg(feature = "allocator-mimalloc")]
	#[test]
	fn mimalloc_comparison_stats_use_allocator_allocated_bytes() {
		let comparable = AllocatorComparisonStats::from(&MimallocStats {
			version: 1,
			allocated_current: 2048,
			allocated_peak: 4096,
			reserved_current: 8192,
			reserved_peak: 12288,
			committed_current: 4096,
			committed_peak: 6144,
			reset_current: 0,
			purged_current: 0,
			page_committed_current: 0,
			pages_current: 0,
			pages_abandoned_current: 0,
			segments_current: 0,
			segments_abandoned_current: 0,
			threads_current: 0,
			requested_current: 0,
			requested_peak: 0,
			process_rss_current: 3072,
			process_rss_peak: 6144,
			process_commit_current: 4096,
			process_commit_peak: 8192,
			page_faults: 0,
			arenas: 2,
		});

		assert_eq!(comparable.allocated_bytes, Some(2048));
		assert_eq!(comparable.committed_bytes, Some(4096));
		assert_eq!(comparable.allocator_structures, Some(2));
	}
}
