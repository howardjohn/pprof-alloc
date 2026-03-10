use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod cgroups;
pub mod malloc;
mod malloc_info;
mod procmaps;
pub mod smaps;

#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Allocations {
	pub allocated: u64,
	pub freed: u64,
	pub allocations: u64,
	pub frees: u64,
}

impl Allocations {
	pub fn in_use_bytes(&self) -> u64 {
		self.allocated.saturating_sub(self.freed)
	}

	pub fn in_use_allocations(&self) -> u64 {
		self.allocations.saturating_sub(self.frees)
	}
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

	pub(crate) fn snapshot(&self) -> Allocations {
		Allocations {
			allocated: self.allocated.load(Ordering::Relaxed),
			freed: self.freed.load(Ordering::Relaxed),
			allocations: self.allocations.load(Ordering::Relaxed),
			frees: self.frees.load(Ordering::Relaxed),
		}
	}
}

impl From<AtomicAllocations> for Allocations {
	fn from(val: AtomicAllocations) -> Self {
		val.snapshot()
	}
}
