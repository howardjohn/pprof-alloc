use regex::Regex;

lazy_static::lazy_static! {
    pub static ref CGROUP_V2_PATH_RE: Regex = Regex::new(r#"(?m)^0::(/.*)$"#).unwrap();
    pub static ref MEMORY_CURRENT_PATH: String = get_memory_current_path().unwrap_or_else(|_| String::new());
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

pub fn get_memory() -> anyhow::Result<usize> {
    let content = std::fs::read_to_string(&*MEMORY_CURRENT_PATH)?;
    content.trim().parse::<usize>()
      .map_err(|e| anyhow::anyhow!("Failed to parse memory value: {}", e))
}