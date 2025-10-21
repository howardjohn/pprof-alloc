mod bt;
mod pprof;

use crate::bt::TraceInfo;
use backtrace::Backtrace;
use dashmap::DashMap;
use itertools::{Itertools, MinMaxResult};
use malloc_info::Error;
use malloc_info::info::Malloc;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::{BufReader, Cursor};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

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
    /// pointer -> size
    static ref POINTER_MAP: DashMap<usize, usize> = DashMap::new();
    static ref LEAKY_POINTER_MAP: DashMap<usize, usize> = DashMap::new();
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
            let ptr = self.inner.alloc(layout);
            if !ptr.is_null() {
                record_allocation(ptr as usize, layout.size());
            }
            ptr
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if IN_ALLOC.with(|x| x.get()) {
            self.inner.dealloc(ptr, layout);
            return;
        }
        enter_alloc(|| {
            self.inner.dealloc(ptr, layout);
            record_deallocation(ptr as usize, layout.size());
        });
    }
}

static ALLOCATIONS: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn malloc_info() -> Result<Malloc, Error> {
    malloc_info::malloc_info()
}

fn record_allocation(start: usize, size: usize) {
    // let (id, name) = thread_id();
    // println!("Allocating {} bytes [thread {} ({})]", size, id, name);

    let trace = crate::bt::HashedBacktrace::capture();

    POINTER_MAP.entry(start).insert(size);
    LEAKY_POINTER_MAP.entry(start).insert(size);

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

fn record_deallocation(start: usize, size: usize) {
    POINTER_MAP.remove(&start);
    // TODO: TRACE_MAP
}

pub fn generate_fragmentation_map() -> anyhow::Result<()> {
    IN_ALLOC.with(|x| x.set(true));
    let MinMaxResult::MinMax(min, max) = LEAKY_POINTER_MAP.iter().map(|x| *x.key()).minmax() else {
        anyhow::bail!("No allocations recorded");
    };
    let total_ranges = LEAKY_POINTER_MAP
        .iter()
        .map(|x| (*x.key(), *x.key() + *x.value()))
        .sorted_by_key(|x| x.0)
        .collect::<Vec<_>>();
    let filled_ranges = POINTER_MAP
        .iter()
        .map(|x| (*x.key(), *x.key() + *x.value()))
        .sorted_by_key(|x| x.0)
        .collect::<Vec<_>>();
    IN_ALLOC.with(|x| x.set(false));
    println!("Min: {}, Max: {}", min, max);
    println!(
        "Total size {} (64 buckets: {})",
        max - min,
        (max - min) / 64
    );
    println!("{total_ranges:?}");
    let diffs = total_ranges
        .iter()
        .zip(total_ranges.iter().skip(1))
        .map(|(a, b)| if b.0 > a.1 { b.0 - a.1 } else { 0 })
        .collect_vec();
    println!("{diffs:?}");
    let logical_size: u64 = total_ranges.iter().map(|(s, e)| (e - s) as u64).sum();
    println!("Total segments: {}", total_ranges.len());
    println!("Logical space size: {}", logical_size);
    // Visualization parameters
    let width = 80;
    let height = 25;
    let total_blocks = width * height;

    // Create buckets for the logical space
    let mut buckets = vec![0u64; total_blocks];
    let bucket_size = logical_size as f64 / total_blocks as f64;

    // Map physical position to logical position (ignoring gaps)
    let mut logical_offset = 0u64;
    let mut segment_map: Vec<(u64, u64, u64)> = Vec::new(); // (phys_start, phys_end, logical_start)

    for (start, end) in &total_ranges {
        segment_map.push((*start as u64, *end as u64, logical_offset));
        logical_offset += (end - start) as u64;
    }


    // Process filled ranges
    for (fstart, fend) in &filled_ranges {
        // Find which segment this filled range belongs to
        if let Some((_, _, log_start)) = segment_map.iter()
          .find(|(s, e, _)| *fstart as u64 >= *s && (*fend as u64) <= *e) {

            let seg_start = segment_map.iter()
              .find(|(s, e, _)| *fstart as u64 >= *s && (*fend as u64) <= *e)
              .map(|(s, _, _)| *s)
              .unwrap();

            // Convert to logical coordinates
            let logical_start = log_start + ((*fstart as u64) - seg_start);
            let logical_end = log_start + ((*fend as u64) - seg_start);

            // Fill buckets
            let start_bucket = (logical_start as f64 / bucket_size) as usize;
            let end_bucket = ((logical_end as f64 / bucket_size).ceil() as usize).min(total_blocks);

            for i in start_bucket..end_bucket {
                let bucket_log_start = (i as f64 * bucket_size) as u64;
                let bucket_log_end = ((i + 1) as f64 * bucket_size) as u64;

                let overlap_start = logical_start.max(bucket_log_start);
                let overlap_end = logical_end.min(bucket_log_end);

                if overlap_end > overlap_start {
                    buckets[i] += overlap_end - overlap_start;
                }
            }
        }
    }

    // Calculate coverage
    let filled: u64 = filled_ranges.iter().map(|(s, e)| (e - s) as u64).sum();
    let coverage = (filled as f64 / logical_size as f64) * 100.0;
    println!("Coverage: {:.2}%\n", coverage);

    // Find max density for normalization
    let max_density = bucket_size as u64;

    // Shading characters
    let shades = [' ', '░', '▒', '▓', '█'];

    // Print visualization
    println!("┌{}┐", "─".repeat(width));
    for row in 0..height {
        print!("│");
        for col in 0..width {
            let idx = row * width + col;
            let density = buckets[idx];

            let shade_idx = if density == 0 {
                0
            } else {
                let normalized = (density as f64 / max_density as f64 * (shades.len() - 1) as f64).ceil() as usize;
                normalized.min(shades.len() - 1)
            };

            print!("{}", shades[shade_idx]);
        }
        println!("│");
    }
    println!("└{}┘", "─".repeat(width));

    println!("\nLegend: {} = empty, {} = partial, {} = full", shades[0], shades[2], shades[4]);
    println!("\nNote: Each segment from the total space is packed sequentially,");
    println!("      ignoring the gaps between them.");

    Ok(())
}
pub fn generate_pprof() -> anyhow::Result<Vec<u8>> {
    IN_ALLOC.with(|x| x.set(true));
    let mut s = String::new();
    s.push_str("heap_v2/1\n");
    s.push_str("  t*: 1: 100 [0: 0");
    for mut entry in TRACE_MAP.iter_mut() {
        s.push_str(&format!("@ {:?}\n", entry.backtrace));
        s.push_str(&format!(
            "  t*: {}: {} [0: 0]\n",
            entry.allocations, entry.allocated
        ));
    }
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
