use anyhow::anyhow;
use prometheus_client::collector::Collector;
use prometheus_client::encoding::DescriptorEncoder;
use prometheus_client::metrics::counter::ConstCounter;
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
	r.working_set = r.usage.saturating_sub(r.inactive_file);
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
	pub file_mapped: u64,
	pub active_file: u64,
	pub inactive_file: u64,
	pub shmem: u64,
	/// Kernel memory.
	pub kernel: u64,
	pub kernel_stack: u64,
	pub pagetables: u64,
	pub percpu: u64,
	pub sock: u64,
	pub slab: u64,
	pub slab_reclaimable: u64,
	pub slab_unreclaimable: u64,
	pub pgfault: u64,
	pub pgmajfault: u64,
	pub workingset_refault_anon: u64,
	pub workingset_refault_file: u64,
	pub workingset_activate_anon: u64,
	pub workingset_activate_file: u64,
	pub workingset_restore_anon: u64,
	pub workingset_restore_file: u64,
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
				"file_mapped" => res.file_mapped = value,
				"active_file" => res.active_file = value,
				"inactive_file" => res.inactive_file = value,
				"shmem" => res.shmem = value,
				"kernel" => res.kernel = value,
				"kernel_stack" => res.kernel_stack = value,
				"pagetables" => res.pagetables = value,
				"percpu" => res.percpu = value,
				"sock" => res.sock = value,
				"slab" => res.slab = value,
				"slab_reclaimable" => res.slab_reclaimable = value,
				"slab_unreclaimable" => res.slab_unreclaimable = value,
				"pgfault" => res.pgfault = value,
				"pgmajfault" => res.pgmajfault = value,
				"workingset_refault_anon" => res.workingset_refault_anon = value,
				"workingset_refault_file" => res.workingset_refault_file = value,
				"workingset_activate_anon" => res.workingset_activate_anon = value,
				"workingset_activate_file" => res.workingset_activate_file = value,
				"workingset_restore_anon" => res.workingset_restore_anon = value,
				"workingset_restore_file" => res.workingset_restore_file = value,
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

		fn encode_gauge(
			encoder: &mut DescriptorEncoder,
			value: u64,
			name: &'static str,
			help: &str,
		) -> Result<(), Error> {
			let metric = ConstGauge::new(value);
			let metric_encoder = encoder.encode_descriptor(name, help, None, metric.metric_type())?;
			metric.encode(metric_encoder)?;
			Ok(())
		}

		fn encode_counter(
			encoder: &mut DescriptorEncoder,
			value: u64,
			name: &'static str,
			help: &str,
		) -> Result<(), Error> {
			let metric = ConstCounter::new(value);
			let metric_encoder = encoder.encode_descriptor(name, help, None, metric.metric_type())?;
			metric.encode(metric_encoder)?;
			Ok(())
		}

		encode_gauge(
			&mut encoder,
			s.usage,
			"cgroup_usage",
			"current memory usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.working_set,
			"cgroup_working_set",
			"current working set",
		)?;
		encode_gauge(
			&mut encoder,
			s.anon,
			"cgroup_anon",
			"current anonymous usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.inactive_anon,
			"cgroup_inactive_anon",
			"current inactive anonymous usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.active_anon,
			"cgroup_active_anon",
			"current active anonymous usage",
		)?;
		encode_gauge(&mut encoder, s.file, "cgroup_file", "current file usage")?;
		encode_gauge(
			&mut encoder,
			s.file_mapped,
			"cgroup_file_mapped",
			"current mapped file usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.active_file,
			"cgroup_active_file",
			"current active file usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.inactive_file,
			"cgroup_inactive_file",
			"current inactive file usage",
		)?;
		encode_gauge(&mut encoder, s.shmem, "cgroup_shmem", "current shmem usage")?;
		encode_gauge(
			&mut encoder,
			s.kernel,
			"cgroup_kernel",
			"current kernel usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.kernel_stack,
			"cgroup_kernel_stack",
			"current kernel stack usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.pagetables,
			"cgroup_pagetables",
			"current pagetables usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.percpu,
			"cgroup_percpu",
			"current percpu usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.sock,
			"cgroup_sock",
			"current socket memory usage",
		)?;
		encode_gauge(&mut encoder, s.slab, "cgroup_slab", "current slab usage")?;
		encode_gauge(
			&mut encoder,
			s.slab_reclaimable,
			"cgroup_slab_reclaimable",
			"current reclaimable slab usage",
		)?;
		encode_gauge(
			&mut encoder,
			s.slab_unreclaimable,
			"cgroup_slab_unreclaimable",
			"current unreclaimable slab usage",
		)?;
		encode_counter(
			&mut encoder,
			s.pgfault,
			"cgroup_pgfault_total",
			"cgroup page faults",
		)?;
		encode_counter(
			&mut encoder,
			s.pgmajfault,
			"cgroup_pgmajfault_total",
			"cgroup major page faults",
		)?;
		encode_counter(
			&mut encoder,
			s.workingset_refault_anon,
			"cgroup_workingset_refault_anon_total",
			"anonymous workingset refaults",
		)?;
		encode_counter(
			&mut encoder,
			s.workingset_refault_file,
			"cgroup_workingset_refault_file_total",
			"file workingset refaults",
		)?;
		encode_counter(
			&mut encoder,
			s.workingset_activate_anon,
			"cgroup_workingset_activate_anon_total",
			"anonymous workingset activations",
		)?;
		encode_counter(
			&mut encoder,
			s.workingset_activate_file,
			"cgroup_workingset_activate_file_total",
			"file workingset activations",
		)?;
		encode_counter(
			&mut encoder,
			s.workingset_restore_anon,
			"cgroup_workingset_restore_anon_total",
			"anonymous workingset restores",
		)?;
		encode_counter(
			&mut encoder,
			s.workingset_restore_file,
			"cgroup_workingset_restore_file_total",
			"file workingset restores",
		)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::MemoryStat;

	#[test]
	fn parse_memory_stat_includes_non_heap_signals() {
		let input = "\
anon 4096
inactive_anon 1024
active_anon 3072
file 8192
file_mapped 2048
active_file 4096
inactive_file 4096
shmem 512
kernel 1024
kernel_stack 128
pagetables 256
percpu 64
sock 32
slab 768
slab_reclaimable 512
slab_unreclaimable 256
pgfault 100
pgmajfault 3
workingset_refault_anon 7
workingset_refault_file 11
workingset_activate_anon 5
workingset_activate_file 13
workingset_restore_anon 2
workingset_restore_file 17";

		let stats = MemoryStat::parse(input).expect("memory.stat should parse");
		assert_eq!(stats.anon, 4096);
		assert_eq!(stats.file_mapped, 2048);
		assert_eq!(stats.shmem, 512);
		assert_eq!(stats.slab_reclaimable, 512);
		assert_eq!(stats.slab_unreclaimable, 256);
		assert_eq!(stats.pgfault, 100);
		assert_eq!(stats.pgmajfault, 3);
		assert_eq!(stats.workingset_refault_file, 11);
		assert_eq!(stats.workingset_restore_file, 17);
	}
}
