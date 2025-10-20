use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

pub struct PprofAlloc {
    inner: System,
}

impl PprofAlloc {
    pub const fn new() -> Self {
        PprofAlloc { inner: System }
    }
}

unsafe impl GlobalAlloc for PprofAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = self.inner.alloc(layout);
        if !ptr.is_null() {
            record_allocation(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record_deallocation(layout.size());
        self.inner.dealloc(ptr, layout);
    }
}

static ALLOCATIONS: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn record_allocation(size: usize) {
    // let bt = Backtrace::new();
    // let key = format!("{:?}", bt);
    // let mut map = ALLOCATIONS.lock().unwrap();
    // *map.entry(key).or_insert(0) += size;
}

fn record_deallocation(_size: usize) {
    // For simplicity, just track allocations, not deallocations
    // In real pprof, we track live memory
}

pub fn generate_pprof() {
    // Placeholder for generating pprof
    // println!("Generating pprof...");
    // let map = ALLOCATIONS.lock().unwrap();
    // for (stack, size) in map.iter() {
    //     println!("Stack: {}, Size: {}", stack, size);
    // }
}
