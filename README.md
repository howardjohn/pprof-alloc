# pprof-alloc

`pprof-alloc` is an experimental Rust crate for getting deeper memory visibility than "heap bytes in use".

The long-term goal is to make allocator behavior observable across three layers:

- stack-attributed allocation profiles in `pprof` format
- allocator-internal state and retention signals for glibc, jemalloc, and mimalloc
- process and cgroup memory residency from Linux

Today, the repository is a working prototype with useful building blocks, but it is not yet a complete allocator observability system.

The wrapper pprof allocator is best treated as a debug-mode opt-in. It records
sampled allocation ownership in user space and can noticeably degrade allocation
and deallocation throughput. For low-overhead production heap profiles, prefer
the jemalloc allocator with jemalloc's native profiler enabled.

## Current status

What exists today:

- A custom `#[global_allocator]` wrapper, `PprofAlloc`, that can record allocation stacks and coarse counters.
- `pprof` export for captured stacks, including Linux shared-object mappings and build IDs for offline symbolization.
- Linux memory collectors for:
  - glibc `malloc_info`
  - cgroup v2 `memory.current` and `memory.stat`, including slab, shmem, and workingset/page-fault signals
  - `/proc/self/smaps_rollup`, including dirty, hugepage, and swap-related process rollups
- Prometheus collectors for the Linux memory views above.
- An `allocation_patterns` example that exercises a range of allocation behaviors.

What is still incomplete:

- Stack-attributed profiles include cumulative allocated bytes and estimated live heap ownership, but they depend on sampled allocation tracking rather than allocator-native heap iteration.
- The allocator wrapper can wrap any `GlobalAlloc`, while allocator-specific stats and collection are currently implemented for declared glibc, jemalloc, and mimalloc backends.
- The crate is intended to be embedded in a binary that exposes HTTP/debug endpoints; the library itself does not need to own the HTTP surface.
- The Linux memory stats are Linux-only; stack capture uses a fast default `frame-pointer` feature on Linux x86_64/aarch64 and a slower `backtrace` fallback elsewhere.

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
- Stack capture uses a manual frame-pointer walk when the default `frame-pointer` feature is active on Linux x86_64/aarch64, and the `backtrace` crate elsewhere.

2. pprof export

- Captured stacks are converted into a gzipped `pprof` protobuf.
- On Linux, loaded ELF mappings and GNU build IDs are included so profiles can be symbolized offline.

3. Linux memory stats

- `stats::malloc` exposes parsed `malloc_info` output from glibc.
- `stats::cgroups` exposes cgroup v2 memory usage, reclaimable vs unreclaimable kernel memory, and page-fault/workingset counters.
- `stats::smaps` exposes rollup data from `/proc/self/smaps_rollup`, including dirty, swap, and hugepage signals.

## Quick start

For low-overhead production heap profiles, build with `allocator-jemalloc` and
use the environment-selected allocator:

```rust
#[global_allocator]
static GLOBAL: pprof_alloc::PprofAlloc =
    pprof_alloc::PprofAlloc::new()
        .with_default(pprof_alloc::Allocator::Jemalloc)
        .with_pprof()
        .with_stats();
```

Run with `PPROF_ALLOC_ALLOCATOR=jemalloc`. Leave `PPROF_ALLOC_BACKEND` unset to
use jemalloc's native heap profiler, or set `PPROF_ALLOC_BACKEND=wrapper` to use
the Rust wrapper sampler for debugging.

With `allocator-jemalloc`, the application owns jemalloc initialization config.
Set `malloc_conf` or `MALLOC_CONF` before allocator initialization with options
such as `prof:true,prof_accum:true`. Then call `pprof_alloc::configure()`
during startup to apply `PPROF_ALLOC_BACKEND` to jemalloc's runtime
`prof.active` setting and `PPROF_ALLOC_SAMPLE_RATE` to jemalloc's runtime
sample rate when jemalloc is active. The allocator hot path does not perform
mallctl configuration.

If you use a non-system builder default and call startup configuration before
the first allocation, pass the same default explicitly:

```rust
pprof_alloc::configure_with_default(pprof_alloc::Allocator::Jemalloc)?;
```

The wrapper sampler records allocations the same way Go's heap profiler does by
default: one sampled allocation per `512 KiB` of allocated bytes on average. It
still has per-allocation and sampled-deallocation overhead, so use
`with_pprof_sample_rate(1)` only for short debug runs. Use
`with_pprof_sample_rate(0)` to disable wrapper pprof recording while still
allowing other allocator stats.

The sample rate can also be deferred to an environment variable while keeping the
global allocator initializer const:

```rust
#[global_allocator]
static GLOBAL: pprof_alloc::PprofAlloc =
    pprof_alloc::PprofAlloc::new()
        .with_pprof_sample_rate_from_env(pprof_alloc::DEFAULT_PPROF_SAMPLE_RATE)
        .with_stats();
```

Set `PPROF_ALLOC_SAMPLE_RATE` before process startup. For the wrapper sampler,
the value is read lazily on the first profiled allocation; missing or invalid
values fall back to the default passed to `with_pprof_sample_rate_from_env`. For
native jemalloc profiling, `configure()` reads the same env var during startup,
rounds it up to jemalloc's nearest power-of-two sample period, and applies it
with `prof.reset`. A value of `0` leaves native profiling inactive.

Set `PPROF_ALLOC_ALLOCATOR=system`, `PPROF_ALLOC_ALLOCATOR=jemalloc`, or
`PPROF_ALLOC_ALLOCATOR=mimalloc` before startup. `ALLOCATOR` is also accepted as
a fallback name. The selection is read once on first allocator use, so it cannot
be changed safely after the process has started. `jemalloc` requires the
`allocator-jemalloc` feature and `mimalloc` requires `allocator-mimalloc`; if an
uncompiled allocator is requested, the process exits with an allocator
configuration error. Enable both allocator features if the same binary should be
able to choose either allocator at runtime.

When built with `allocator-jemalloc`, `PPROF_ALLOC_BACKEND` selects
the pprof backend at runtime when the active allocator is jemalloc. Leave it
unset, or set any value other than `wrapper`, `pprof-alloc`, or `rust`, to use
jemalloc's native heap profiler. Set `PPROF_ALLOC_BACKEND=wrapper` before
startup to use `pprof-alloc`'s wrapper sampler instead.

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

The allocator section of the snapshot is split into:

- `comparable`: normalized cross-allocator fields for head-to-head comparison
- `specific`: allocator-specific detail for deeper debugging

`declare_allocator_kind!(...)` registers the allocator kind once at process startup, so you declare the global allocator and its kind side by side without doing any setup in `main()` and without touching allocator metadata in the hot path.

If you omit `declare_allocator_kind!(...)`, the crate now reports the allocator as `undeclared` instead of silently defaulting to glibc. In that state, allocator comparison metrics are withheld and the snapshot surface reports an explicit configuration error.

Register Prometheus collectors:

```rust
let mut registry = prometheus_client::registry::Registry::default();
pprof_alloc::allocator::PrometheusCollector::register(&mut registry);
pprof_alloc::stats::cgroups::PrometheusCollector::register(&mut registry);
pprof_alloc::stats::smaps::PrometheusCollector::register(&mut registry);
```

`stats::malloc::PrometheusCollector` is still available, but it is glibc-specific and not the right surface for cross-allocator comparison.

`allocator::PrometheusCollector` always exports `allocator_info{allocator="..."}` and `allocator_configured`, and only exports normalized allocator byte metrics when that backend can provide the corresponding field without inventing a fake zero.

Run the example:

```bash
cargo run --example allocation_patterns
```

## Frame-pointer mode

On Linux x86_64 and Linux aarch64, stack capture uses a manual frame-pointer
walk. That mode requires frame pointers to be preserved:

```bash
RUSTFLAGS=-Cforce-frame-pointers=yes cargo run --example allocation_patterns
```

Current caveats:

- This is the intended fast path for stack capture.
- The fast path is controlled by the default `frame-pointer` feature.
- Build with `default-features = false` to force the slower `backtrace` crate fallback, even on Linux x86_64/aarch64.
- Non-Linux targets and unsupported architectures use the slower `backtrace` crate fallback regardless of features.
- It performs best-effort stack-bounds and frame-chain validation. Invalid or
  missing frame pointers may truncate the captured stack.

You can check the active unwinder at runtime with:

```rust
println!("{:?}", pprof_alloc::capture_mode());
```

## Benchmarking Capture Overhead

The repo includes a simple allocation benchmark for the active unwinder:

```bash
cargo run --example capture_benchmark
```

The benchmark reports the active capture mode and a few allocation-heavy workloads.

## Reading the outputs

The crate is most useful when you compare the memory views instead of treating any single number as "truth".

- `pprof`: stack-attributed bytes with both `alloc_space` and `inuse_space` sample types in one profile.
- `stats::malloc`: allocator-managed memory according to glibc, including arena/system totals.
- `stats::smaps`: what the kernel says is resident, anonymous, dirty, swapped, or backed by huge pages for the process.
- `stats::cgroups`: what the container cgroup is currently charged for, including anon/file/kernel splits and reclaim/refault pressure signals.

Those differences are where fragmentation, retention, cached pages, and allocator policy start to show up.

## Limitations

- Linux-first implementation.
- `malloc_info` requires glibc and only reflects glibc allocator state.
- Fast stack capture on Linux x86_64 and Linux aarch64 works best when frame
  pointers are present and valid. Invalid frame-pointer chains are truncated;
  disable default features or use other targets to use the slower fallback
  unwinder.
- Sampling is process-wide for the active global allocator and assumes the rate is
  effectively constant for a captured profile, matching pprof's heap profile
  model.

## Repository layout

- `src/lib.rs`: allocator wrapper and profile generation entry point
- `src/trace/`: stack capture
- `src/pprof/`: profile encoding and mapping/build ID discovery
- `src/stats/`: Linux memory and allocator stats
- `examples/allocation_patterns.rs`: example workload
