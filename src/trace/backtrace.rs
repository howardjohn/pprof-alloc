use smallvec::SmallVec;

pub fn trace() -> super::UnresolvedFrames {
	let mut bt: SmallVec<[u64; super::SOFT_MAX_DEPTH]> =
		SmallVec::with_capacity(super::SOFT_MAX_DEPTH);
	// Safety: not sure, it doesn't say why unsynchronized is unsafe. pprof-rs does this...
	unsafe {
		backtrace::trace_unsynchronized(|f| {
			bt.push(f.ip() as u64);
			true
		});
	}
	bt.into()
}
