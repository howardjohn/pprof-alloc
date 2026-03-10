# Roadmap

## Goal

Build a memory analysis toolkit that explains allocator behavior, not just heap size.

The target outcome is a system that can answer questions like:

- which call stacks are responsible for current live memory?
- how much memory has the allocator reserved beyond live allocations?
- how much of that retention is fragmentation, arenas, dirty pages, caches, or page-level residency?
- how do those answers differ across allocators under the same workload?

## Scope decisions

These are now explicit project constraints:

- glibc, jemalloc, and mimalloc are first-class allocator targets
- the library owns instrumentation, snapshots, and collectors
- the consuming binary owns HTTP/debug endpoint exposure

## Current baseline

The repository already has a useful foundation:

- global allocation interception
- stack capture and `pprof` export
- Linux mapping/build ID capture
- glibc `malloc_info` parsing
- cgroup v2 memory stats
- `smaps_rollup` parsing
- Prometheus collectors for the Linux stat views

The main gap is that the core allocation accounting is still prototype-grade: frees are not applied back to the owning stack, so the profile does not yet represent true live memory.

## Priority 0: Make the core profile correct

This is the highest-value work because every downstream analysis depends on it.

Tasks:

- Track pointer -> allocation metadata for every live allocation.
- On free, subtract bytes and allocation count from the originating stack.
- Handle `alloc_zeroed` and `realloc` explicitly.
- Decide whether the default exported sample should be:
  - true `inuse_space`
  - cumulative `alloc_space`
  - or both
- Replace hash-only equality with collision-safe backtrace identity.
- Make profile generation snapshot-safe and reentrancy-safe.

Acceptance criteria:

- A workload that allocates and then frees memory produces near-zero live bytes in the exported profile.
- `pprof` output is semantically aligned with the sample labels.
- There are tests for alloc/free/realloc correctness.

## Priority 1: Expose consistent snapshot APIs

Right now the repo has useful internals but not a clean "give me the current state" surface.

Tasks:

- Add public snapshot structs for:
  - live allocation profile metadata
  - coarse allocator counters
  - glibc allocator stats
  - process RSS/PSS residency
  - cgroup memory charge
- Add a single combined snapshot API so callers can collect all views at one timestamp.
- Add serialization-friendly types for debug endpoints and offline capture.

Acceptance criteria:

- A caller can retrieve a coherent memory snapshot with one API call.
- The example can dump one combined JSON snapshot plus a `pprof` profile.

## Priority 2: Add integration-friendly snapshot and collector surfaces

The consuming binary will own HTTP, so the library should focus on making that integration easy and consistent.

Tasks:

- Add stable APIs that make it trivial for a binary to expose:
  - a `pprof` heap profile
  - a combined memory snapshot
  - Prometheus metrics
- Ensure snapshot types and error types are ergonomic to serialize and serve from an app-owned endpoint layer.
- Add a stable JSON schema for the debug snapshot endpoint.
- Document recommended endpoint shapes, caching, and security expectations for binaries embedding the crate.

Acceptance criteria:

- A consuming binary can expose pprof, metrics, and JSON state with minimal glue code.
- The library does not require an opinionated HTTP stack.

## Priority 3: Make glibc, jemalloc, and mimalloc first-class

The repo vision is allocator comparison, which requires a more explicit allocator abstraction.

Tasks:

- Define an abstraction for allocator-specific introspection hooks.
- Keep the tracing layer allocator-agnostic.
- Add first-class allocator backends/adapters for:
  - glibc / ptmalloc
  - jemalloc
  - mimalloc
- Normalize allocator-specific concepts into comparable high-level metrics where possible.
- Preserve allocator-specific deep stats instead of flattening everything into generic counters.

Acceptance criteria:

- The same workload can be built against glibc, jemalloc, and mimalloc with comparable exported diagnostics.
- Allocator-specific details remain available without forcing everything into a lowest-common-denominator model.

## Priority 4: Model fragmentation and retained memory explicitly

This is the core differentiator for the project.

Tasks:

- Define a first-class model for:
  - live bytes
  - allocator-reserved bytes
  - mapped bytes
  - resident bytes
  - retained-but-unused bytes
  - fragmentation estimates
- Derive explanatory metrics such as:
  - allocator overhead ratio
  - reserved/live ratio
  - resident/live ratio
  - dirty-page retention
- For allocators that expose arena/bin/span details, add per-arena or per-size-class diagnostics.
- Add periodic snapshotting so trends can be compared over time, not just at one instant.

Acceptance criteria:

- The crate can explain why RSS stays high after heap drops.
- A comparison run can highlight allocator policy differences, not just raw totals.

## Priority 5: Make comparison and validation rigorous

This project needs trustworthy experiments, not just instrumentation.

Tasks:

- Build benchmark workloads that stress:
  - churn
  - long-lived objects
  - mixed-size lifetimes
  - thread-local caches
  - fragmentation-heavy patterns
- Add golden checks comparing:
  - exported live bytes
  - allocator-reported totals
  - RSS/PSS/cgroup residency
- Measure instrumentation overhead for:
  - default backtrace mode
  - frame-pointer mode
  - different stack depths
- Add CI coverage for Linux environments where the procfs- and glibc-based features are expected to work.

Acceptance criteria:

- The repo can report both memory behavior and instrumentation overhead.
- Regressions in accounting correctness are caught automatically.

## Priority 6: Production hardening

Tasks:

- Add feature gating so expensive components can be enabled independently.
- Introduce sampling and/or rate controls for high-allocation-rate services.
- Bound memory growth of internal tracking structures.
- Improve failure handling for missing procfs, cgroup, or allocator capabilities.
- Document platform support and unsupported modes clearly.

Acceptance criteria:

- The crate can run in a service without unbounded self-overhead.
- Operators can choose between low-overhead coarse stats and high-fidelity deep inspection.

## Suggested near-term sequence

1. Fix live allocation accounting and sample semantics.
2. Add a combined snapshot API.
3. Add a minimal debug HTTP example exposing profile + JSON + metrics.
4. Add jemalloc and mimalloc backends alongside the current glibc path.
5. Add benchmark workloads and validation harnesses.

## Open implementation questions

- What is the best allocator abstraction shape for mixing common metrics with allocator-specific deep state?
- Which jemalloc and mimalloc control/stat surfaces should be considered required for parity with the glibc path?
- Is the primary deployment target Linux services in containers?
- What overhead budget is acceptable for always-on mode versus incident/debug mode?
