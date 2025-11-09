use smallvec::SmallVec;
use std::arch::asm;

// With 'tracefp', I see pretty poor performance with unless memory-access-check is disabled
// hopframe is better, but does more work that isn't really needed
// This implementation is like tracefp but faster since we don't use libc getcontext and instead just
// inline the 2 registers we need.

pub fn trace() -> super::UnresolvedFrames {
	let mut bt: SmallVec<[u64; super::SOFT_MAX_DEPTH]> =
		SmallVec::with_capacity(super::SOFT_MAX_DEPTH);
	let mut pc = 0;
	let mut fp = 0;
	unsafe {
		asm!("lea {}, [rip]", out(reg) pc);
		asm!("mov {}, rbp", out(reg) fp);
	}
	bt.push(pc);
	while fp != 0 {
		pc = load::<u64>(fp + 8);
		pc -= 1;
		bt.push(pc);
		fp = load::<u64>(fp);
	}
	bt.into()
}

#[inline]
fn load<T: Copy>(address: u64) -> T {
	unsafe { *(address as *const T) }
}
