mod bt;
mod pprof;

use backtrace::Backtrace;
use malloc_info::Error;
use malloc_info::info::Malloc;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::{BufReader, Cursor};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use dashmap::DashMap;
use crate::bt::TraceInfo;

pub struct PprofAlloc {
    inner: System,
}

impl PprofAlloc {
    pub const fn new() -> Self {
        PprofAlloc { inner: System }
    }
}

fn enter_alloc<T>(func: impl FnOnce() -> T) -> T {
    let current_value = IN_ALLOC.with(|x| x.get());
    IN_ALLOC.with(|x| x.set(true));
    let output = func();
    IN_ALLOC.with(|x| x.set(current_value));
    output
}

/// next thread id incrementor
static THREAD_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static THREAD_ID: usize = THREAD_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    static THREAD_NAME: Option<Arc<str>> = std::thread::current().name().map(|s| Arc::from(s));
    /// Used to avoid recursive alloc/dealloc calls for interior allocation
    static IN_ALLOC: Cell<bool> = Cell::new(false);
}


lazy_static::lazy_static! {
    /// pointer -> data
    // static ref PTR_MAP: DashMap<usize, PointerData> = DashMap::new();
    // backtrace -> current allocation size
    static ref TRACE_MAP: DashMap<u64, TraceInfo> = DashMap::new();
}


fn thread_id() -> (usize, Arc<str>) {
    (
        THREAD_ID.with(|id| *id),
        THREAD_NAME.with(|n| n.clone()).unwrap_or_default(),
    )
}

unsafe impl GlobalAlloc for PprofAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if IN_ALLOC.with(|x| x.get()) {
            return self.inner.alloc(layout);
        }
        enter_alloc(|| {
            let size = layout.size();
            let ptr = self.inner.alloc(layout);
            if !ptr.is_null() {
                record_allocation(layout.size());
            }
            ptr
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record_deallocation(layout.size());
        self.inner.dealloc(ptr, layout);
    }
}

static ALLOCATIONS: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn malloc_info() -> Result<Malloc, Error> {
    malloc_info::malloc_info()
}

fn record_allocation(size: usize) {
    let (id, name) = thread_id();
    // println!("Allocating {} bytes [thread {} ({})]", size, id, name);

    let trace = crate::bt::HashedBacktrace::capture();

    let mut trace_info = TRACE_MAP.entry(trace.hash()).or_insert_with(|| TraceInfo {
        backtrace: trace,
        allocated: 0,
        freed: 0,
        allocations: 0,
    });
    trace_info.allocated += size as u64;
    trace_info.allocations += 1;
    // let bt = Backtrace::new();
    // let key = format!("{:?}", bt);
    // let mut map = ALLOCATIONS.lock().unwrap();
    // *map.entry(key).or_insert(0) += size;
}

fn record_deallocation(_size: usize) {
    // For simplicity, just track allocations, not deallocations
    // In real pprof, we track live memory
}

pub fn generate_pprof() -> anyhow::Result<Vec<u8>>  {
    IN_ALLOC.with(|x| x.set(true));
    let mut s = String::new();
    s.push_str("heap_v2/1\n");
    for mut entry in TRACE_MAP.iter_mut() {
        s.push_str(&format!("@ {:?}\n", entry.backtrace));
        s.push_str(&format!("t*: {}: {} [0: 0]\n", entry.allocations, entry.allocated));
    }
    eprintln!("{s}");
    eprintln!("HELLO");
    IN_ALLOC.with(|x| x.set(false));
    let profile = pprof::parse_jeheap(Cursor::new(s))?;
    let pprof = profile.to_pprof(("inuse_space", "bytes"), ("space", "bytes"), None);
    Ok(pprof)
    // pprof::parse_jeheap()
    // Placeholder for generating pprof
    // println!("Generating pprof...");
    // let map = ALLOCATIONS.lock().unwrap();
    // for (stack, size) in map.iter() {
    //     println!("Stack: {}, Size: {}", stack, size);
    // }
}
