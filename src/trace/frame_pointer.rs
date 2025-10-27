use smallvec::SmallVec;
use std::arch::asm;
use std::cell::RefCell;

thread_local! {
    static UNWINDER: RefCell<hopframe::unwinder::StackUnwinder>  = RefCell::new(hopframe::unwinder::UnwindBuilder::new().build());
}

// pub fn trace() -> super::UnresolvedFrames {
//     let mut bt: SmallVec<[usize; super::SOFT_MAX_DEPTH]> =
//         SmallVec::with_capacity(super::SOFT_MAX_DEPTH);
//     UNWINDER.with(|unwinder| {
//         let mut unwinder = unwinder.borrow_mut();
//         let i = unwinder.unwind();
//         i.for_each(|f| bt.push(f.address() as usize))
//     });
//     bt.into()
// // }
// pub fn trace() -> super::UnresolvedFrames {
//     let mut bt: SmallVec<[u64; super::SOFT_MAX_DEPTH]> =
//         SmallVec::with_capacity(super::SOFT_MAX_DEPTH);
//     // Safety: not sure, it doesn't say why unsynchronized is unsafe. pprof-rs does this...
//     unsafe {
//         tracefp::trace(|f| {
//             bt.push(f);
//             true
//         });
//     }
//     bt.into()
// }
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
