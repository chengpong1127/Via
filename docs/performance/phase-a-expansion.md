# Sequential Phase A evaluation

Phase A removes the P2 work counter as a termination condition. Every prepared
corridor is evaluated in its existing order unless a real error or deadline
prevents completion. P1, frontier/candidate caps, transfer and duration checks,
provider limits, Matrix dimensions, and development/production deadlines retain
their existing behavior.

## Responsibilities and Phase B boundary

`ExpansionInputs` borrows the immutable network, retained corridor set, timing
snapshot, and query. `ExpansionResolver::expand_state_sequential` runs Transit,
Walking, and BikeShare in that order. It returns `ExpansionOutcome`, including
local work statistics and provider failure classification even on a fatal result.
Transit, Walking, and BikeShare now each return an `ExpansionOutcome`; the state
orchestrator merges them in movement order and stops on a fatal result. The
evaluator aggregates state outcomes before error propagation and prunes once per
edge. The follow-up cleanup comparison is recorded in
[outcome-cleanup.md](outcome-cleanup.md).

`transit_ready_at`, `transit_next_state`, `walking_next_state`, and
`bike_next_states` are deterministic functions without provider access, cache
mutation, statistics, deadline reads, or mutable evaluator access. They reuse
`EvaluationState::after_movement`, `bike_departure`, and `after_bike_share`.
Transfer-count rejection remains in finalization, preserving the original policy.

`ExpansionResolver::resolve_walking` executes walking resolution through
`WalkingCache::realize`, which calls `StreetRouter::matrix` on a miss.
`expand_state_sequential` executes BikeShare resolution through
`BikeShareSession::realize`; the existing BikeShare planner performs endpoint
walking and bicycle Matrix I/O. `ExpansionInputs::resolve_transit` reads the
request-local timing snapshot and performs no HTTP requests. Pure transition
functions consume the resolved data and construct successor states.

The mutable dependencies remaining are `WalkingCache` and `BikeShareSession`,
including its candidate and exact-result caches. Independent states can already
compute transitions from the same immutable edge and inputs. Their I/O execution
still borrows these request-local caches exclusively.

Phase B will need to separate cache lookup/update from provider execution,
define deterministic handling of overlapping misses and cached failures, return
cache/statistics effects to the collector, and bound concurrent state work.
Results must merge in original state order before the existing single pruning
pass. Keep each BikeShare request's dependent endpoint walks, reachable-station
filtering, and bicycle matrices ordered. Cancellation/deadline behavior and
provider concurrency limits must survive that change. Phase A introduces no
synchronization, new cache, Matrix batching change, or parallel state execution.

## Deterministic measurements

Baseline: `60b203b70bd14c0f83664982068932be4b3ae28b`.
All measurements use the same release profile, in-memory walking provider,
fresh request-local cache per iteration, and fixed departure instant. There are
five warmup iterations and twenty measured iterations per workload, repeated in
five runs with baseline/reference/refactored order rotated between runs. The
benchmark binaries are pinned to CPU 7. The latency table reports the median of
the five per-run medians, and the median of their five p95 values. A 60-second
external watchdog wraps each benchmark invocation. No live TDX load was used.

The two workloads inject 32 or 2,400 identical prepared direct-walking corridors
into the evaluator. This deliberately bypasses P1 and its static corridor cap to
isolate evaluation and cross the old boundary cheaply. It is an evaluator stress
fixture, not an estimate of production corridor counts or latency. Deduplication
leaves one final candidate in both cases.

The reference is an external copy of the baseline with only the benchmark query's
P2 budget raised to 100,000. The capped and high-budget binaries use the original
transition and cache implementation. Raw logs retain the old charged-operation
counter name for historical interpretation; new `states_expanded` counts input
states, independently of movement attempts.

| Workload / implementation | Median P2 µs | Median p95 µs | Corridors completed | Walking Matrix requests | Walking cache hits | Final candidates |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A: 32 corridors, capped baseline | 40 | 48 | 32 | 1 | 31 | 1 |
| A: 32 corridors, high-budget reference | 35 | 50 | 32 | 1 | 31 | 1 |
| A: 32 corridors, Phase A | 42 | 53 | 32 | 1 | 31 | 1 |
| B: 2,400 corridors, capped baseline | 2,296 | 2,784 | 2,000 | 1 | 1,999 | 1 |
| B: 2,400 corridors, high-budget reference | 2,929 | 3,306 | 2,400 | 1 | 2,399 | 1 |
| B: 2,400 corridors, Phase A | 2,979 | 3,318 | 2,400 | 1 | 2,399 | 1 |

Group A's median increases by 2 µs (5%). Group B's median increases by 683 µs
(29.7%) relative to the truncated baseline, but only 50 µs (1.7%) relative to
the reference doing the same complete workload. These fixture measurements show
approximately equivalent refactor cost, with extra total work after removal.
All runs have zero transit operations/lookups, BikeShare operations, bicycle
Matrix requests, and BikeShare cache hits.

| Workload / implementation | Corridors attempted | Input states processed | Walking operations | Successors generated |
| --- | ---: | ---: | ---: | ---: |
| A: baseline / reference / Phase A | 32 | 32 | 32 | 32 |
| B: capped baseline | 2,001 | 2,001 | 2,000 | 2,000 |
| B: high-budget reference / Phase A | 2,400 | 2,400 | 2,400 | 2,400 |

Raw measurements: [baseline](phase-a-expansion/baseline.txt),
[high-budget reference](phase-a-expansion/reference.txt),
[Phase A](phase-a-expansion/after.txt).

For Group A, successful walking operations, completed corridors, successors,
Matrix requests, cache hits, and the final fingerprint remain identical.
For Group B, evaluating all corridors adds exactly 400 successful walking
operations, 400 successors, and 400 cache hits (20% more completed work), with
no additional Matrix requests. The old evaluator also enters one failing state
expansion on corridor 2,001 before rejecting its walking charge. Thus baseline
state-expansion attempts are 2,001, versus 2,400 after removal; the old charge
counter reports 2,000 rather than that state count. These baseline successor and
attempt counts follow directly from the one-edge fixture's control flow.

All capped, high-budget reference, and refactored runs produce this fingerprint:

```text
direct-walking:1789984800000000000:300:2000:destination
```

It represents a single 300-second, 2,000-metre walk. Its departure, arrival,
movement metrics, and final candidate count are unchanged. Group B produces more
completed corridors without a different winner because the inputs are duplicates.
Other workloads may select different winners when previously skipped corridors
complete; that is the intended effect of removing the budget.

Timings are small in-process fixture measurements and can vary with host load.
Compare Group B primarily with the high-budget reference for refactor cost;
compare with the capped baseline to quantify additional work. These walking-only
fingerprints do not establish equivalence for every transit/BikeShare request.
Existing deterministic transit, rental, cache, reconstruction, ranking, provider
failure, and deadline tests provide the broader regression coverage.

Run the retained benchmark with:

```bash
timeout 60s cargo test --release --manifest-path server/Cargo.toml --lib \
  phase_a_deterministic_benchmark -- --ignored --nocapture
```

The transit-only/direct-plus-transit fixture remains runnable with
`direct_corridor_has_observable_constant_work`. It checks one additional state
and walking Matrix request without changing departure lookups. Its 100-iteration
release measurements are retained in [before](phase-a-expansion/transit-before.txt)
and [after](phase-a-expansion/transit-after.txt). The original metric counted
5/6 movement charges; the new metric counts 4/5 input states. Those values have
different meanings and are not compared as a change in actual work.

## Verification

All required commands pass:

```bash
cargo fmt --manifest-path server/Cargo.toml --check
CARGO_BUILD_JOBS=2 cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features -- -D warnings
CARGO_BUILD_JOBS=2 cargo test --manifest-path server/Cargo.toml --all-targets --all-features
git diff --check
```

The all-target suite passes 387 tests, with three opt-in benchmarks ignored.
The new benchmark was also executed explicitly. The release library suite passes
191 tests; the original reference library suite passes 188. New coverage includes
pure Walking/Transit/BikeShare transitions, immutable input preservation, invalid
arrivals, duration/latest-arrival rejection, rental extension, more than 2,000
operations, corridor completion, cache reuse, and rejection of the obsolete TOML
field. Existing transfer, per-edge cap, provider failure, reconstruction/ranking,
and development/production deadline regressions remain green.

No routing differences were observed in retained deterministic regressions beyond
updated expectations for the removed work budget. The former direct-only fake
BikeShare provider was restricted to endpoint requests: its old implementation
returned invalid anchor indexes for transit edges that the budget previously
prevented from completing. Production BikeShare resolution is unchanged.

The standards and specification review passes found no blocking issues. A
successor-counter edge case identified in review was corrected: transit
successors are counted as constructed, including work before a later deadline
error. No source comments, concurrency primitives, provider batching changes,
commit, merge, or deployment were introduced.
