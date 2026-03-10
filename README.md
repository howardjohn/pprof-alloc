# pprof-alloc

`pprof-alloc` is an experimental Rust crate for getting deeper memory visibility than "heap bytes in use".

The long-term goal is to make allocator behavior observable across three layers:

- stack-attributed allocation profiles in `pprof` format
- allocator-internal state and retention signals for glibc, jemalloc, and mimalloc
- process and cgroup memory residency from Linux

Today, the repository is a working prototype with useful building blocks, but it is not yet a complete allocator observability system.

## Current status

What exists today:

- A custom `#[global_allocator]` wrapper, `PprofAlloc`, that can record allocation stacks and coarse counters.
- `pprof` export for captured stacks, including Linux shared-object mappings and build IDs for offline symbolization.
- Linux memory collectors for:
  - glibc `malloc_info`
  - cgroup v2 `memory.current` and `memory.stat`
  - `/proc/self/smaps_rollup`
- Prometheus collectors for the Linux memory views above.
- An `allocation_patterns` example that exercises a range of allocation behaviors.

What is still incomplete:

- Deallocations are not yet applied to stack attribution, so the exported `pprof` data is cumulative allocated bytes by stack, not true live heap ownership.
- The allocator wrapper currently wraps `std::alloc::System`; it is not yet a generic adapter for glibc, jemalloc, and mimalloc.
- The crate is intended to be embedded in a binary that exposes HTTP/debug endpoints; the library itself does not need to own the HTTP surface.
- The Linux stats are strongest on glibc-based environments; non-Linux support is limited.

## Why this exists

Allocator behavior is usually more interesting than the heap size reported by the application runtime.

To understand real memory cost, you typically need to compare:

- logical heap growth by call site
- allocator-reserved memory
- process RSS/PSS and anonymous pages
- cgroup working set and file cache

This crate is aiming to make those views available together so you can reason about fragmentation, retained pages, allocator arena growth, and "where did the memory actually go?"

The intended deployment model is:

- this crate provides allocator instrumentation, snapshots, and metrics collectors
- the application binary decides how to expose those through HTTP or other operational surfaces

## Architecture

The current crate has three main pieces:

1. Allocation tracing

- `PprofAlloc` wraps the global allocator and records backtraces on allocation.
- Stack capture uses a manual frame-pointer walk.

2. pprof export

- Captured stacks are converted into a gzipped `pprof` protobuf.
- On Linux, loaded ELF mappings and GNU build IDs are included so profiles can be symbolized offline.

3. Linux memory stats

- `stats::malloc` exposes parsed `malloc_info` output from glibc.
- `stats::cgroups` exposes cgroup v2 memory usage and working-set-style fields.
- `stats::smaps` exposes rollup data from `/proc/self/smaps_rollup`.

## Quick start

Use the allocator wrapper as your global allocator:

```rust
#[global_allocator]
static GLOBAL: pprof_alloc::PprofAlloc =
    pprof_alloc::PprofAlloc::new().with_pprof().with_stats();
```

Generate a profile:

```rust
let profile = pprof_alloc::generate_pprof()?;
std::fs::write("/tmp/pprof.memprof", profile)?;
```

Read Linux memory state:

```rust
let malloc = pprof_alloc::stats::malloc::info()?;
let cgroup = pprof_alloc::stats::cgroups::get_stats()?;
let smaps = pprof_alloc::stats::smaps::rollup()?;
```

Capture one combined snapshot for a JSON/debug endpoint:

```rust
let snapshot = pprof_alloc::snapshot();
let json = serde_json::to_string_pretty(&snapshot)?;
```

Register Prometheus collectors:

```rust
let mut registry = prometheus_client::registry::Registry::default();
pprof_alloc::stats::malloc::PrometheusCollector::register(&mut registry);
pprof_alloc::stats::cgroups::PrometheusCollector::register(&mut registry);
pprof_alloc::stats::smaps::PrometheusCollector::register(&mut registry);
```

Run the example:

```bash
cargo run --example allocation_patterns
```

## Frame-pointer mode

Frame-pointer unwinding requires frame pointers to be preserved:

```bash
RUSTFLAGS="-Cforce-frame-pointers=yes" cargo run --example allocation_patterns
```

Current caveats:

- This is the intended fast path for stack capture.
- The frame-pointer unwinder is architecture-specific.
- It assumes a valid frame-pointer chain and does not yet have defensive bounds checking.

You can check the active unwinder at runtime with:

```rust
println!("{:?}", pprof_alloc::capture_mode());
```

## Benchmarking Capture Overhead

The repo includes a simple allocation benchmark for the frame-pointer unwinder:

```bash
RUSTFLAGS="-Cforce-frame-pointers=yes" cargo run --example capture_benchmark
```

The benchmark reports the active capture mode and a few allocation-heavy workloads.

## Reading the outputs

The crate is most useful when you compare the memory views instead of treating any single number as "truth".

- `pprof`: stack-attributed bytes with both `alloc_space` and `inuse_space` sample types in one profile.
- `stats::malloc`: allocator-managed memory according to glibc, including arena/system totals.
- `stats::smaps`: what the kernel says is resident and anonymous for the process.
- `stats::cgroups`: what the container cgroup is currently charged for, including working-set-style fields.

Those differences are where fragmentation, retention, cached pages, and allocator policy start to show up.

## Limitations

- Linux-first implementation.
- `malloc_info` requires glibc and only reflects glibc allocator state.
- Stack capture assumes frame pointers are present and valid.
- There is no sampling, rate limiting, or production-tuned overhead model yet.

## Repository layout

- `src/lib.rs`: allocator wrapper and profile generation entry point
- `src/trace/`: stack capture
- `src/pprof/`: profile encoding and mapping/build ID discovery
- `src/stats/`: Linux memory and allocator stats
- `examples/allocation_patterns.rs`: example workload

## Roadmap

See [ROADMAP.md](./ROADMAP.md) for the concrete work plan.
