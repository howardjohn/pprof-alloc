use procsmaps::{Mapping, VmFlags};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Default, PartialEq)]
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
    let smaps = procsmaps::from_str(&input).expect("library never returns None");
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
