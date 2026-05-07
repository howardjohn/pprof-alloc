use smallvec::SmallVec;
use std::arch::asm;
use std::cell::Cell;
use std::mem;
use std::ptr;

// With 'tracefp', I see pretty poor performance with unless memory-access-check is disabled
// hopframe is better, but does more work that isn't really needed
// This implementation is like tracefp but faster since we don't use libc getcontext and instead just
// inline the 2 registers we need.

const WORD_SIZE: usize = mem::size_of::<usize>();
const FRAME_RECORD_SIZE: usize = 2 * WORD_SIZE;
const FRAME_POINTER_ALIGNMENT_MASK: usize = mem::align_of::<usize>() - 1;

thread_local! {
	static STACK_BOUNDS: Cell<Option<StackBounds>> = const { Cell::new(None) };
}

pub fn trace() -> super::UnresolvedFrames {
	let mut bt: SmallVec<[u64; super::SOFT_MAX_DEPTH]> =
		SmallVec::with_capacity(super::SOFT_MAX_DEPTH);
	let mut pc = 0;
	let mut fp = 0;
	unsafe {
		#[cfg(target_arch = "x86_64")]
		{
			asm!("lea {}, [rip]", out(reg) pc);
			asm!("mov {}, rbp", out(reg) fp);
		}

		#[cfg(target_arch = "aarch64")]
		{
			asm!("adr {}, .", out(reg) pc);
			asm!("mov {}, x29", out(reg) fp);
		}
	}
	bt.push(pc);
	let bounds = stack_bounds();
	while bt.len() < super::SOFT_MAX_DEPTH && bounds.contains_frame(fp) {
		let return_address = load_return_address(fp);
		let Some(pc) = return_address_to_call_pc(return_address) else {
			break;
		};
		let next_fp = load_saved_frame_pointer(fp);
		bt.push(pc);
		if !frame_chain_makes_progress(fp, next_fp) {
			break;
		};
		fp = next_fp;
	}
	bt.into()
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn return_address_to_call_pc(pc: u64) -> Option<u64> {
	pc.checked_sub(1)
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn return_address_to_call_pc(pc: u64) -> Option<u64> {
	pc.checked_sub(4)
}

#[inline]
fn load<T: Copy>(address: usize) -> T {
	unsafe { *(address as *const T) }
}

#[inline]
fn load_saved_frame_pointer(fp: usize) -> usize {
	load::<usize>(fp)
}

#[inline]
fn load_return_address(fp: usize) -> u64 {
	load::<usize>(fp + WORD_SIZE) as u64
}

#[inline]
fn frame_chain_makes_progress(fp: usize, next_fp: usize) -> bool {
	next_fp > fp
}

#[derive(Clone, Copy, Debug)]
struct StackBounds {
	low: usize,
	max_frame: usize,
}

impl StackBounds {
	#[inline]
	fn contains_frame(self, fp: usize) -> bool {
		fp >= self.low && fp <= self.max_frame && fp & FRAME_POINTER_ALIGNMENT_MASK == 0
	}
}

fn stack_bounds() -> StackBounds {
	STACK_BOUNDS.with(|cached| {
		if let Some(bounds) = cached.get() {
			return bounds;
		}
		let bounds = current_thread_stack_bounds();
		cached.set(Some(bounds));
		bounds
	})
}

fn current_thread_stack_bounds() -> StackBounds {
	let mut attr = mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
	if unsafe { libc::pthread_getattr_np(libc::pthread_self(), attr.as_mut_ptr()) } != 0 {
		return empty_stack_bounds();
	}
	let mut attr = unsafe { attr.assume_init() };

	let mut stack_addr = ptr::null_mut();
	let mut stack_size = 0usize;
	let result = unsafe { libc::pthread_attr_getstack(&attr, &mut stack_addr, &mut stack_size) };
	unsafe {
		libc::pthread_attr_destroy(&mut attr);
	}

	if result != 0 {
		return empty_stack_bounds();
	}
	let low = stack_addr as usize;
	let Some(high) = low.checked_add(stack_size) else {
		return empty_stack_bounds();
	};
	let Some(max_frame) = high.checked_sub(FRAME_RECORD_SIZE) else {
		return empty_stack_bounds();
	};
	if low == 0 || max_frame < low {
		return empty_stack_bounds();
	}
	StackBounds { low, max_frame }
}

const fn empty_stack_bounds() -> StackBounds {
	StackBounds {
		low: 1,
		max_frame: 0,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn stack_bounds_require_room_for_frame_record() {
		let bounds = StackBounds {
			low: 0x1000,
			max_frame: 0x10f0,
		};

		assert!(bounds.contains_frame(0x1000));
		assert!(bounds.contains_frame(0x10f0));
		assert!(!bounds.contains_frame(0x10f8));
	}

	#[test]
	fn stack_bounds_reject_unaligned_frame_pointers() {
		let bounds = StackBounds {
			low: 0x1000,
			max_frame: 0x10f0,
		};

		assert!(!bounds.contains_frame(0x1001));
	}

	#[test]
	fn stack_bounds_reject_empty_bounds() {
		assert!(!empty_stack_bounds().contains_frame(0x1000));
	}

	#[test]
	fn frame_chain_requires_progress() {
		assert!(frame_chain_makes_progress(0x1000, 0x1010));
		assert!(!frame_chain_makes_progress(0x1000, 0x1000));
		assert!(!frame_chain_makes_progress(0x1000, 0x0ff0));
	}

	#[test]
	fn stack_bounds_reject_frame_above_max_frame() {
		let bounds = StackBounds {
			low: 0x1000,
			max_frame: 0x10f0,
		};

		assert!(!bounds.contains_frame(0x10f8));
	}
}
