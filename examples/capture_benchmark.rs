use std::hint::black_box;
use std::time::{Duration, Instant};

#[global_allocator]
static GLOBAL: pprof_alloc::PprofAlloc = pprof_alloc::PprofAlloc::new().with_pprof();

const WARMUP_ITERS: usize = 25_000;
const TINY_BOX_ITERS: usize = 250_000;
const MEDIUM_VEC_ITERS: usize = 100_000;
const REALLOC_ITERS: usize = 60_000;

fn main() {
	println!("capture_mode={:?}", pprof_alloc::capture_mode());
	println!("Build with RUSTFLAGS=-Cforce-frame-pointers=yes for correct unwinding.");
	println!();

	warmup();

	run_case("tiny_box_churn", TINY_BOX_ITERS, tiny_box_churn);
	run_case("medium_vec_churn", MEDIUM_VEC_ITERS, medium_vec_churn);
	run_case("realloc_churn", REALLOC_ITERS, realloc_churn);

	let stats = pprof_alloc::allocation_stats();
	println!();
	println!(
		"allocation_stats allocated={} freed={} in_use_bytes={}",
		stats.allocated,
		stats.freed,
		stats.in_use_bytes()
	);
}

fn warmup() {
	for i in 0..WARMUP_ITERS {
		tiny_box_churn(i);
	}
}

fn run_case(name: &str, iterations: usize, mut func: impl FnMut(usize)) {
	let start = Instant::now();
	for i in 0..iterations {
		func(i);
	}
	let elapsed = start.elapsed();
	let ns_per_op = elapsed.as_nanos() as f64 / iterations as f64;
	let ops_per_sec = iterations as f64 / elapsed.as_secs_f64();
	println!(
		"{name} iterations={iterations} elapsed={} ns_per_op={ns_per_op:.1} ops_per_sec={ops_per_sec:.0}",
		fmt_duration(elapsed),
	);
}

fn tiny_box_churn(i: usize) {
	let value = black_box(Box::new([i as u64; 8]));
	black_box(value[0]);
}

fn medium_vec_churn(i: usize) {
	let len = 256 + (i % 512);
	let mut data = Vec::with_capacity(len);
	for j in 0..len {
		data.push(((i + j) & 0xff) as u8);
	}
	black_box(data.len());
	black_box(data.as_ptr());
}

fn realloc_churn(i: usize) {
	let mut data = Vec::with_capacity(8);
	for j in 0..(64 + (i % 64)) {
		data.push(((i ^ j) & 0xff) as u8);
	}
	black_box(data.capacity());
}

fn fmt_duration(duration: Duration) -> String {
	if duration.as_secs() > 0 {
		format!("{:.3}s", duration.as_secs_f64())
	} else if duration.as_millis() > 0 {
		format!("{:.3}ms", duration.as_secs_f64() * 1_000.0)
	} else {
		format!("{}us", duration.as_micros())
	}
}
