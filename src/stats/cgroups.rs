use anyhow::anyhow;
use prometheus_client::collector::Collector;
use prometheus_client::encoding::DescriptorEncoder;
use prometheus_client::metrics::gauge::ConstGauge;
use regex::Regex;
use serde::Serialize;
use std::fmt::Error;

lazy_static::lazy_static! {
		static ref CGROUP_V2_PATH_RE: Regex = Regex::new(r#"(?m)^0::(/.*)$"#).unwrap();
		static ref MEMORY_CURRENT_PATH: String = get_memory_current_path().unwrap_or_else(|_| String::new());
		static ref MEMORY_STAT_PATH: String = get_memory_stat_path().unwrap_or_else(|_| String::new());
}

fn get_cgroupv2_path() -> anyhow::Result<String> {
	let cgroup_path = String::from_utf8(std::fs::read("/proc/self/cgroup")?)?;

	CGROUP_V2_PATH_RE
		.captures(&cgroup_path)
		.map(|x| format!("/sys/fs/cgroup{}", x.get(1).unwrap().as_str()))
		.ok_or_else(|| anyhow::anyhow!("Failed to parse cgroup path"))
}

fn get_memory_current_path() -> anyhow::Result<String> {
	let path = get_cgroupv2_path()?;
	Ok(format!("{}/memory.current", path))
}

fn get_memory_stat_path() -> anyhow::Result<String> {
	let path = get_cgroupv2_path()?;
	Ok(format!("{}/memory.stat", path))
}

pub fn get_memory() -> anyhow::Result<u64> {
	let content = std::fs::read_to_string(&*MEMORY_CURRENT_PATH)?;
	content
		.trim()
		.parse::<u64>()
		.map_err(|e| anyhow::anyhow!("Failed to parse memory value: {}", e))
}

pub fn get_stats() -> anyhow::Result<MemoryStat> {
	let content = std::fs::read_to_string(&*MEMORY_STAT_PATH)?;
	let mut r = MemoryStat::parse(content.trim())
		.map_err(|e| anyhow::anyhow!("Failed to parse memory stat: {}", e))?;
	r.usage = get_memory()?;
	r.working_set = r.usage - r.inactive_file;
	Ok(r)
}

/// A few interesting values from memory.stat
#[derive(Default, Debug, Clone, Serialize)]
pub struct MemoryStat {
	pub usage: u64,
	// https://github.com/google/cadvisor/blob/5adb1c3bb38b4c5d50b31f39faf3214a44ae479b/container/libcontainer/handler.go#L847
	pub working_set: u64,
	/// Anonymous memory, inclusive of swap.
	pub anon: u64,
	pub inactive_anon: u64,
	pub active_anon: u64,
	/// File-backed memory.
	pub file: u64,
	pub active_file: u64,
	pub inactive_file: u64,
	/// Kernel memory.
	pub kernel: u64,
}

impl MemoryStat {
	fn parse(content: &str) -> anyhow::Result<Self> {
		let mut res = MemoryStat::default();

		for line in content.lines() {
			let mut parts = line.split_whitespace();
			let key = parts
				.next()
				.ok_or_else(|| anyhow!("Invalid line: '{}' (no key)", line))?;
			let value = parts
				.next()
				.ok_or_else(|| anyhow!("Invalid line: '{}' (no value)", line))?
				.parse::<u64>()
				.map_err(|_| anyhow!("Invalid line: '{}' (invalid value)", line))?;
			if parts.next().is_some() {
				return Err(anyhow!("Invalid line: '{}' (too many parts)", line));
			}

			match key {
				"anon" => res.anon = value,
				"inactive_anon" => res.inactive_anon = value,
				"active_anon" => res.active_anon = value,
				"file" => res.file = value,
				"active_file" => res.active_file = value,
				"inactive_file" => res.inactive_file = value,
				"kernel" => res.kernel = value,
				// Ignore other keys
				_ => {},
			}
		}

		Ok(res)
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
	fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), Error> {
		use prometheus_client::encoding::EncodeMetric;
		let Ok(s) = get_stats() else {
			return Ok(());
		};
		let mut encode = |v: u64, n: &'static str, d: &str| {
			let metric = ConstGauge::new(v);
			let metric_encoder = encoder.encode_descriptor(n, d, None, metric.metric_type())?;
			metric.encode(metric_encoder)?;
			Ok(())
		};
		encode(s.usage, "cgroup_usage", "current memory usage")?;
		encode(s.working_set, "cgroup_working_set", "current working set")?;
		encode(s.anon, "cgroup_anon", "current anonymous usage")?;
		encode(
			s.inactive_anon,
			"cgroup_inactive_anon",
			"current inactive anonymous usage",
		)?;
		encode(
			s.active_anon,
			"cgroup_active_anon",
			"current active anonymous usage",
		)?;
		encode(s.file, "cgroup_file", "current file usage")?;
		encode(
			s.active_file,
			"cgroup_active_file",
			"current active file usage",
		)?;
		encode(
			s.inactive_file,
			"cgroup_inactive_file",
			"current inactive file usage",
		)?;
		encode(s.kernel, "cgroup_kernel", "current kernel usage")?;
		Ok(())
	}
}
