# Outcome-based expansion cleanup

This sequential cleanup starts at `822e7022b5c809bfe827a1fd99a74a5eca6b3e6a` on
`p2-optimization`. It changes outcome ownership and aggregation rather than
routing rules, providers, configuration, caching, or concurrency.

## Resolution and aggregation

`ExpansionInputs::resolve_transit(&self)` returns an operation-local
`ExpansionOutcome`. Its computation inputs are immutable, but its deadline
checks make it effectful. It still reads the same timing snapshot in the same
leg/departure order. `resolve_walking(&mut self)` and
`resolve_bike_share(&mut self)` also return outcomes; their mutable receiver is
required for the existing request-local Walking and BikeShare caches and I/O.
None accepts a caller-owned statistics or failure output parameter.
`resolved_data` returns classified failures alongside optional provider data.

The four pure helpers (`transit_ready_at`, `transit_next_state`,
`walking_next_state`, and `bike_next_states`) remain byte-for-byte unchanged.
They use immutable computation inputs and neither read deadlines nor access
providers, caches, or statistics.

`ExpansionStats::merge` sums operation-local counts into the state outcome.
`EvaluationFailures::merge` combines failure flags, and `ExpansionOutcome::append`
merges both before retaining the operation's nested successor result. The state
orchestrator checks for fatal errors before resolving the next movement mode.
An expired initial deadline records no state attempt; otherwise each state is
counted exactly once. The original deadline checks around movement attempts and
provider resolution remain in the same positions.

The evaluator constructs one resolver per corridor. Its state loop calls
`Phase2EvaluationStats::record_expansion` and merges failure flags **before**
propagating `outcome.successors`. The old mutable `expand_state` wrapper is
removed. Transit successors are counted when constructed, so a later error
preserves their observational count even though the incomplete expansion returns
no usable successors. Operation and global statistics retain distinct scopes.
Per-edge pruning still happens only after all states on that edge succeed.

## Regression coverage

Seven focused tests cover:

- Local Transit/Walking/BikeShare counts, and exact operation-to-state aggregation.
- Identical ordered successor movements after separate versus combined resolution.
- Walking cache hits, including cached provider errors; BikeShare exact cache reuse.
- Transient, internal, and unreachable provider classifications and Walking fallback.
- Fatal Transit errors preventing both surface operations.
- Fatal Walking errors preserving already generated Transit counts and skipping BikeShare.
- A fatal BikeShare deadline retaining an earlier Walking failure.
- Global evaluator counters surviving fatal error propagation.

The existing pure transition, rental, provider, cache, reconstruction, ranking,
frontier, and development/production deadline regressions remain in the suite.

## Measurements

The existing Phase A benchmark covers 32 and 2,400 prepared direct-walking
corridors. The added `outcome_cleanup_multimodal_benchmark` covers a direct
Walking corridor and two identical retained Bus corridors with two departures,
Walking substitution, and two synthetic BikeShare options. Repeated corridor
inputs exercise both Walking and BikeShare cache hits.

Both versions use the same benchmark fixtures, release profile, five warmups,
and twenty measured iterations per workload. The baseline is an external copy
of HEAD with only the new benchmark test added. No production baseline code is
modified. Each invocation has a 60-second external watchdog. All providers are
in-memory fixtures; no live TDX load is used.

Each version was run five times, alternating which version ran first. All
benchmark processes were pinned to CPU 7 and ran their two benchmark tests
sequentially. The table reports the median of five per-run median values and
the median of five per-run p95 values. These are fresh measurements against the
cleanup baseline, not comparisons with the historical Phase A timing table.

| Workload | Before median µs | After median µs | Before p95 µs | After p95 µs |
| --- | ---: | ---: | ---: | ---: |
| 32 Walking corridors | 20 | 21 | 22 | 24 |
| 2,400 Walking corridors | 1,611 | 1,659 | 1,696 | 1,688 |
| Transit / Walking / BikeShare | 26 | 27 | 30 | 34 |

Median changes are 1–48 µs (approximately 3–5%). There is no evidence here of a
meaningful production latency change; these small in-process fixtures measure
architecture-preservation cost rather than real provider performance.

Every following counter is identical before and after, across all five runs:

| Metric per request | Walking ×32 | Walking ×2,400 | Mixed modes |
| --- | ---: | ---: | ---: |
| States expanded | 32 | 2,400 | 15 |
| Transit operations attempted | 0 | 0 | 2 |
| Transit departure lookups | 0 | 0 | 2 |
| Walking operations attempted | 32 | 2,400 | 15 |
| BikeShare operations attempted | 0 | 0 | 2 |
| Successors generated before pruning | 32 | 2,400 | 23 |
| Walking Matrix requests | 1 | 1 | 2 |
| Bicycle Matrix requests | 0 | 0 | 0 |
| Walking cache hits | 31 | 2,399 | 1 |
| BikeShare exact requests | 0 | 0 | 1 |
| BikeShare exact cache hits | 0 | 0 | 1 |
| Corridors attempted / completed | 32 / 32 | 2,400 / 2,400 | 3 / 3 |
| Final P2 candidates | 1 | 1 | 6 |

Both large Walking workloads finish all prepared corridors; their Phase A
baseline already has no P2 work budget. The mixed-mode BikeShare injection uses
the existing exact-result cache seam and performs no physical bicycle Matrix
calls. It measures alternative construction and cache reuse, not network latency.

The comparison script verified all work fields and final journey fingerprints
across versions and repetitions, excluding only elapsed latency. Both Walking
workloads retain `direct-walking:1789984800000000000:300:2000:destination`.
The mixed-mode fixture retains all six ordered candidates: two transit departures,
two BikeShare durations, a Walking replacement, and the direct walk. Its full
fingerprints include departure/arrival timestamps for every movement, so matching
identity strings alone cannot hide timing changes. These are P2 candidate-pool
comparisons; reconstruction and final ranking are covered by the unchanged
routing integration tests.

Raw five-run output: [before](outcome-cleanup/before.txt) and
[after](outcome-cleanup/after.txt). To reproduce the current benchmarks:

```bash
timeout 60s cargo test --release --manifest-path server/Cargo.toml --lib \
  phase_a_deterministic_benchmark -- --ignored --nocapture

timeout 60s cargo test --release --manifest-path server/Cargo.toml --lib \
  outcome_cleanup_multimodal_benchmark -- --ignored --nocapture
```

The baseline was reconstructed with `git archive` at HEAD `822e702`, adding only
the new multimodal benchmark function to its existing evaluator test module.
Its implementation, fixtures, provider mocks, and configuration otherwise match
that commit. No routing-output, Matrix-count, cache, or error-classification
changes were observed in the benchmark and regression workloads.

## Remaining Phase B boundary

A future implementation can collect one `ExpansionOutcome` per state and merge
those outcomes in original state order without changing transition logic. It
must still split mutable cache lookup/update from independent provider work,
define deterministic overlapping-miss and cached-failure handling, bound state
concurrency within existing provider limits, and preserve cancellation/deadlines.
Each state's movement resolution and BikeShare's dependent endpoint-walking and
bicycle Matrix operations retain their required order. Pruning remains once per
completed edge. This cleanup introduces no synchronization or parallel expansion.

## Required checks

All required checks pass on the final source:

```bash
cargo fmt --manifest-path server/Cargo.toml --check
CARGO_BUILD_JOBS=2 cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features -- -D warnings
CARGO_BUILD_JOBS=2 cargo test --manifest-path server/Cargo.toml --all-targets --all-features
git diff --check
```

The final all-target suite passes 394 tests, with four opt-in benchmarks ignored.
Both evaluator benchmarks were also run explicitly in the five-run comparison.
The unmodified baseline all-target suite passed 387 tests. Standards and
specification review found no actionable issues. The changes remain uncommitted;
no merge or deployment was performed.
