# Dashboard Build Prompt

Build a Grafana dashboard for comparing memory behavior across three Kubernetes deployments of the same service, where the only meaningful difference is the allocator:

- one pod name will include `-glibc`
- one pod name will include `-jemalloc`
- one pod name will include `-mimalloc`

The dashboard should be designed for side-by-side allocator comparison in production, using Prometheus data only.

## Goal

The purpose of this dashboard is to understand allocator behavior deeply, not just heap usage. It should make it easy to compare:

- stack-attributed in-use heap from `pprof_alloc`
- allocator-managed bytes
- allocator-reserved / mapped / retained memory
- process RSS/PSS style memory
- cgroup memory and working set

The dashboard should help answer:

- which allocator retains the most memory beyond logical heap usage?
- which allocator maps or commits the most memory for the same workload?
- how much gap is there between heap-attributed bytes and actual resident/container-charged memory?
- how does allocator behavior diverge over time under the same traffic pattern?

## Matching / grouping

Assume pod names can be matched by suffix:

- `.*-glibc.*`
- `.*-jemalloc.*`
- `.*-mimalloc.*`

Use the pod label or pod metric label for grouping. If there is a `namespace` label, preserve it as a dashboard variable.

The dashboard should make it easy to compare the three allocator modes directly, ideally in the same panel when sensible.

## Important metrics to use

Use these `pprof-alloc` metrics when present:

- `allocator_info{allocator="..."}`
- `allocator_allocated_bytes`
- `allocator_active_bytes`
- `allocator_resident_bytes`
- `allocator_mapped_bytes`
- `allocator_retained_bytes`
- `allocator_metadata_bytes`
- `allocator_committed_bytes`
- `allocator_structures`

Use these cgroup metrics:

- `cgroup_usage`
- `cgroup_working_set`
- `cgroup_anon`
- `cgroup_active_anon`
- `cgroup_inactive_anon`
- `cgroup_file`
- `cgroup_active_file`
- `cgroup_inactive_file`
- `cgroup_kernel`

Use these process rollup metrics:

- any `smaps_*` metrics exported by this repo’s `stats::smaps::PrometheusCollector`

Important constraint:

- allocator metrics are intentionally sparse
- if a field is not meaningful for a given allocator, it may be absent rather than zero
- the dashboard should handle missing series gracefully

## Required dashboard structure

Create panels for these categories.

### 1. Allocator identity and deployment sanity

Include a small top row that clearly shows which pods are currently matched as:

- glibc
- jemalloc
- mimalloc

Also include a panel using `allocator_info` so it is obvious if a pod is mislabeled or a deployment is not exporting the expected allocator identity.

### 2. Head-to-head core memory comparison

Create time series panels comparing the three allocator modes for:

- `allocator_allocated_bytes`
- `allocator_mapped_bytes`
- `allocator_resident_bytes`
- `allocator_committed_bytes`
- `allocator_retained_bytes`
- `cgroup_usage`
- `cgroup_working_set`
- key `smaps` resident metrics

Where it helps, combine related series into one panel. Prefer readable comparison over excessive panel count.

### 3. Gap / fragmentation style views

Create panels or derived queries that highlight important gaps, such as:

- `cgroup_usage - allocator_allocated_bytes`
- `allocator_mapped_bytes - allocator_allocated_bytes`
- `allocator_resident_bytes - allocator_allocated_bytes`
- working set versus allocator-attributed bytes

These are important because the whole point is to understand retention, fragmentation, and memory that exists beyond the logical in-use heap.

### 4. Allocator-internal comparison

Create panels for allocator internals where available:

- `allocator_active_bytes`
- `allocator_metadata_bytes`
- `allocator_structures`

Handle absent series cleanly for allocators that do not expose a given field.

### 5. Container and process memory comparison

Create panels that compare allocator-level metrics against:

- cgroup usage / working set
- `smaps` RSS/PSS style metrics
- anonymous vs file-backed memory

The goal is to show how allocator behavior translates into actual kernel/container memory cost.

## Query and panel expectations

- Prefer panels that overlay glibc, jemalloc, and mimalloc in a single chart when they represent the same concept.
- Use legends that clearly identify allocator mode from pod name or allocator label.
- Use units in bytes with sensible IEC formatting.
- Handle missing series without making the dashboard look broken.
- If a panel is derived from multiple metrics and one allocator lacks one of them, degrade gracefully.
- Use dashboard variables for at least:
  - namespace
  - workload or app selector if available
  - pod regex override if useful

## Deliverables

Produce:

1. A Grafana dashboard JSON.
2. A short note describing:
   - the variables used
   - how the three allocator groups are matched
   - any assumptions about metric labels
   - any panels that intentionally tolerate missing allocator metrics

## Important design intent

This is not a generic service dashboard. It is a memory-comparison dashboard focused on allocator behavior. Optimize for:

- comparing glibc vs jemalloc vs mimalloc at a glance
- identifying retained/mapped/resident deltas
- showing where allocator-level and kernel/container-level views diverge

