#[cfg(not(feature = "frame-pointer"))]
use backtrace::trace;
#[cfg(feature = "frame-pointer")]
use frame_pointer::trace;

use itertools::Itertools;
use smallvec::SmallVec;
use std::fmt;
use std::hash::{Hash, Hasher};

mod backtrace;
mod frame_pointer;

const SOFT_MAX_DEPTH: usize = 128;

#[derive(Clone)]
struct UnresolvedFrames(SmallVec<[u64; SOFT_MAX_DEPTH]>);

impl From<SmallVec<[u64; SOFT_MAX_DEPTH]>> for UnresolvedFrames {
	fn from(x: SmallVec<[u64; SOFT_MAX_DEPTH]>) -> Self {
		Self(x)
	}
}

#[derive(Clone)]
pub struct HashedBacktrace {
	inner: UnresolvedFrames,
	hash: u64,
}

impl fmt::Debug for HashedBacktrace {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let address = self.inner.0.iter().map(|x| format!("{:#x}", x)).join(" ");
		f.write_str(&address)
	}
}

impl HashedBacktrace {
	pub fn capture() -> Self {
		let bt = trace();
		let mut hasher = ahash::AHasher::default();
		bt.0.iter().for_each(|x| hasher.write_u64(*x));
		let hash = hasher.finish();
		Self { inner: bt, hash }
	}
	pub fn addrs(&self) -> Vec<u64> {
		self.inner.0.iter().copied().collect_vec()
	}
}

impl PartialEq for HashedBacktrace {
	fn eq(&self, other: &Self) -> bool {
		self.hash == other.hash && self.inner.0 == other.inner.0
	}
}

impl Eq for HashedBacktrace {}

impl Hash for HashedBacktrace {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.inner.0.iter().for_each(|x| state.write_u64(*x));
	}
}
