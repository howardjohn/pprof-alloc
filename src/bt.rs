use backtrace::Backtrace;
use itertools::Itertools;
use std::fmt;
use std::fmt::Debug;
use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Clone)]
pub struct HashedBacktrace {
    inner: Backtrace,
    hash: u64,
}

pub struct TraceInfo {
    pub backtrace: HashedBacktrace,
    pub allocated: u64,
    pub freed: u64,
    pub allocations: u64,
}

impl fmt::Debug for HashedBacktrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let address = self
            .inner
            .frames()
            .into_iter()
            .map(|x| format!("{:#x}", x.ip() as usize))
            .join(" ");
        f.write_str(&address)
    }
}

impl HashedBacktrace {
    pub fn capture() -> Self {
        let backtrace = Backtrace::new_unresolved();
        let mut hasher = DefaultHasher::new();
        backtrace
            .frames()
            .iter()
            .for_each(|x| hasher.write_u64(x.ip() as u64));
        let hash = hasher.finish();
        Self {
            inner: backtrace,
            hash,
        }
    }

    pub fn inner(&self) -> &Backtrace {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut Backtrace {
        &mut self.inner
    }

    pub fn hash(&self) -> u64 {
        self.hash
    }
}

impl PartialEq for HashedBacktrace {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

impl Eq for HashedBacktrace {}

impl Hash for HashedBacktrace {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}
