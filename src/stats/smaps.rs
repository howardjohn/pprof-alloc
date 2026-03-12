use prometheus_client::collector::Collector;
use prometheus_client::encoding::DescriptorEncoder;
use prometheus_client::metrics::gauge::ConstGauge;
use serde::Serialize;
use std::fmt::Error;
use std::fs::File;
use std::io::Read;

#[derive(Debug, Default, PartialEq, Clone, Serialize)]
pub struct ProcessStats {
	pub size: u64,
	pub rss: u64,
	pub pss: u64,
	pub pss_dirty: u64,
	pub shared_clean: u64,
	pub shared_dirty: u64,
	pub private_clean: u64,
	pub private_dirty: u64,
	pub referenced: u64,
	pub anonymous: u64,
	pub lazy_free: u64,
	pub anon_huge_pages: u64,
	pub shmem_huge_pages: u64,
	pub shmem_pmd_mapped: u64,
	pub file_pmd_mapped: u64,
	pub shared_hugetlb: u64,
	pub private_hugetlb: u64,
	pub swap: u64,
	pub swap_pss: u64,
	pub locked: u64,
}

pub fn rollup() -> anyhow::Result<ProcessStats> {
	let path = "/proc/self/smaps_rollup";
	let mut file = File::open(path)?;
	let mut input = String::new();
	file.read_to_string(&mut input)?;
	parse_rollup(&input)
}

fn parse_rollup(input: &str) -> anyhow::Result<ProcessStats> {
	let smaps = super::procmaps::from_str(&input).expect("library never returns None");
	if smaps.len() != 1 {
		return Err(anyhow::anyhow!(
			"Expected 1 smaps entry, got {}",
			smaps.len()
		));
	}
	let smap = smaps.into_iter().next().unwrap();
	Ok(ProcessStats {
		size: smap.size,
		rss: smap.rss,
		pss: smap.pss,
		pss_dirty: smap.pss_dirty,
		shared_clean: smap.shared_clean,
		shared_dirty: smap.shared_dirty,
		private_clean: smap.private_clean,
		private_dirty: smap.private_dirty,
		referenced: smap.referenced,
		anonymous: smap.anonymous,
		lazy_free: smap.lazy_free,
		anon_huge_pages: smap.anon_huge_pages,
		shmem_huge_pages: smap.shmem_huge_pages,
		shmem_pmd_mapped: smap.shmem_pmd_mapped,
		file_pmd_mapped: smap.file_pmd_mapped,
		shared_hugetlb: smap.shared_hugetlb,
		private_hugetlb: smap.private_hugetlb,
		swap: smap.swap,
		swap_pss: smap.swap_pss,
		locked: smap.locked,
	})
}

#[derive(Debug, Clone)]
pub struct PrometheusCollector {}

impl PrometheusCollector {
	pub fn register(registry: &mut prometheus_client::registry::Registry) {
		registry.register_collector(Box::new(Self {}))
	}
}

impl Collector for PrometheusCollector {
	fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), Error> {
		use prometheus_client::encoding::EncodeMetric;
		let Ok(s) = rollup() else {
			return Ok(());
		};
		let mut encode = |v: u64, n: &'static str, d: &str| {
			let metric = ConstGauge::new(v);
			let metric_encoder = encoder.encode_descriptor(n, d, None, metric.metric_type())?;
			metric.encode(metric_encoder)?;
			Ok(())
		};
		encode(s.size, "process_size", "size memory usage")?;
		encode(s.rss, "process_rss", "rss memory usage")?;
		encode(s.pss, "process_pss", "pss memory usage")?;
		encode(s.pss_dirty, "process_pss_dirty", "pss_dirty memory usage")?;
		encode(
			s.shared_clean,
			"process_shared_clean",
			"shared_clean memory usage",
		)?;
		encode(
			s.shared_dirty,
			"process_shared_dirty",
			"shared_dirty memory usage",
		)?;
		encode(
			s.private_clean,
			"process_private_clean",
			"private_clean memory usage",
		)?;
		encode(
			s.private_dirty,
			"process_private_dirty",
			"private_dirty memory usage",
		)?;
		encode(
			s.referenced,
			"process_referenced",
			"referenced memory usage",
		)?;
		encode(s.anonymous, "process_anonymous", "anonymous memory usage")?;
		encode(s.lazy_free, "process_lazy_free", "lazy free memory usage")?;
		encode(
			s.anon_huge_pages,
			"process_anon_huge_pages",
			"anonymous huge pages usage",
		)?;
		encode(
			s.shmem_huge_pages,
			"process_shmem_huge_pages",
			"shared memory huge pages usage",
		)?;
		encode(
			s.shmem_pmd_mapped,
			"process_shmem_pmd_mapped",
			"shared memory pmd mapped usage",
		)?;
		encode(
			s.file_pmd_mapped,
			"process_file_pmd_mapped",
			"file pmd mapped usage",
		)?;
		encode(
			s.shared_hugetlb,
			"process_shared_hugetlb",
			"shared hugetlb usage",
		)?;
		encode(
			s.private_hugetlb,
			"process_private_hugetlb",
			"private hugetlb usage",
		)?;
		encode(s.swap, "process_swap", "process swap usage")?;
		encode(
			s.swap_pss,
			"process_swap_pss",
			"process proportional swap usage",
		)?;
		encode(s.locked, "process_locked", "process locked memory usage")?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::{ProcessStats, parse_rollup};

	#[test]
	fn parse_rollup_includes_non_heap_process_signals() {
		let input = "\
638000000000-638000001000 ---p 00000000 00:00 0                          [rollup]
Rss:                8192 kB
Pss:                6144 kB
Pss_Dirty:          2048 kB
Shared_Clean:       1024 kB
Shared_Dirty:        512 kB
Private_Clean:      1536 kB
Private_Dirty:      5120 kB
Referenced:         7168 kB
Anonymous:          4096 kB
LazyFree:            256 kB
AnonHugePages:      2048 kB
ShmemHugePages:      128 kB
ShmemPmdMapped:      256 kB
FilePmdMapped:       512 kB
Shared_Hugetlb:       64 kB
Private_Hugetlb:      32 kB
Swap:               1024 kB
SwapPss:             768 kB
Locked:               16 kB
Size:              16384 kB
";

		assert_eq!(
			parse_rollup(input).expect("smaps rollup should parse"),
			ProcessStats {
				size: 16384 * 1024,
				rss: 8192 * 1024,
				pss: 6144 * 1024,
				pss_dirty: 2048 * 1024,
				shared_clean: 1024 * 1024,
				shared_dirty: 512 * 1024,
				private_clean: 1536 * 1024,
				private_dirty: 5120 * 1024,
				referenced: 7168 * 1024,
				anonymous: 4096 * 1024,
				lazy_free: 256 * 1024,
				anon_huge_pages: 2048 * 1024,
				shmem_huge_pages: 128 * 1024,
				shmem_pmd_mapped: 256 * 1024,
				file_pmd_mapped: 512 * 1024,
				shared_hugetlb: 64 * 1024,
				private_hugetlb: 32 * 1024,
				swap: 1024 * 1024,
				swap_pss: 768 * 1024,
				locked: 16 * 1024,
			}
		);
	}
}
