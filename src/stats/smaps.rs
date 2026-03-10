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
}

pub fn rollup() -> anyhow::Result<ProcessStats> {
	let path = "/proc/self/smaps_rollup";
	let mut file = File::open(path)?;
	let mut input = String::new();
	file.read_to_string(&mut input)?;
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
		Ok(())
	}
}
