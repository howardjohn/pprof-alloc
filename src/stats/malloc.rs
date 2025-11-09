use crate::stats::malloc_info;
use crate::stats::malloc_info::Error;
use crate::stats::malloc_info::info::{Malloc, SystemType};
use prometheus_client::collector::Collector;
use prometheus_client::encoding::DescriptorEncoder;
use prometheus_client::metrics::gauge::ConstGauge;

pub fn info() -> Result<MallocInfo, Error> {
	malloc_info::malloc_info().map(MallocInfo)
}

#[cfg(target_os = "linux")]
#[cfg(target_env = "gnu")]
pub fn malloc_trim() {
	unsafe {
		let _ = libc::malloc_trim(0usize);
	}
}

#[derive(Debug)]
pub struct MallocInfo(pub Malloc);

impl MallocInfo {
	pub fn system_max(&self) -> u64 {
		self
			.0
			.system
			.iter()
			.find(|s| s.r#type == SystemType::Max)
			.map(|s| s.size as u64)
			.unwrap_or_default()
	}
	pub fn system_current(&self) -> u64 {
		self
			.0
			.system
			.iter()
			.find(|s| s.r#type == SystemType::Current)
			.map(|s| s.size as u64)
			.unwrap_or_default()
	}
	pub fn total(&self) -> u64 {
		self.0.total.iter().map(|t| (t.size * t.count) as u64).sum()
	}
	pub fn heaps(&self) -> u64 {
		self.0.heaps.len() as u64
	}
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
		use prometheus_client::encoding::EncodeMetric;
		let Ok(s) = info() else {
			return Ok(());
		};
		let mut encode = |v: u64, n: &'static str, d: &str| {
			let metric = ConstGauge::new(v);
			let metric_encoder = encoder.encode_descriptor(n, d, None, metric.metric_type())?;
			metric.encode(metric_encoder)?;
			Ok(())
		};
		encode(s.system_max(), "malloc_max", "total peak memory")?;
		encode(s.system_current(), "malloc_current", "total current memory")?;
		encode(s.heaps(), "malloc_heaps", "current heaps used")?;
		Ok(())
	}
}
