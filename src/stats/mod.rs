use std::sync::atomic::AtomicU64;

pub mod cgroups;
pub mod malloc;
mod malloc_info;
mod procmaps;
pub mod smaps;

#[derive(Default, Clone, Debug)]
pub struct Allocations {
	pub allocated: u64,
	pub freed: u64,
	pub allocations: u64,
	pub frees: u64,
}

#[derive(Default, Debug)]
pub(crate) struct AtomicAllocations {
	pub allocated: AtomicU64,
	pub freed: AtomicU64,
	pub allocations: AtomicU64,
	pub frees: AtomicU64,
}

impl AtomicAllocations {
	pub(crate) const fn new() -> AtomicAllocations {
		AtomicAllocations {
			allocated: AtomicU64::new(0),
			freed: AtomicU64::new(0),
			allocations: AtomicU64::new(0),
			frees: AtomicU64::new(0),
		}
	}
}

impl From<AtomicAllocations> for Allocations {
	fn from(val: AtomicAllocations) -> Self {
		Allocations {
			allocated: val.allocated.load(std::sync::atomic::Ordering::Relaxed),
			freed: val.freed.load(std::sync::atomic::Ordering::Relaxed),
			allocations: val.allocations.load(std::sync::atomic::Ordering::Relaxed),
			frees: val.frees.load(std::sync::atomic::Ordering::Relaxed),
		}
	}
}
