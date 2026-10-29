# Phase D.4b: Intermediate State Pruning

## Change

`prune_states()` now computes `PathMetrics` once for each input state and stores
the state and metrics together in a local `ScoredState`. Stable sorting still
uses `(ready_at, walking_seconds, transfer_count)`. Capability checks, timing
quality, frontier operations, the minimum frontier limit, and returned state
objects are unchanged.

The original pruning function remains available under `cfg(test)` as the
structural reference. The equivalence tests compare ordered `EvaluationState`
values, including movements and active BikeShare rental data.

## Historical isolated benchmark

The pre-edit release baseline captured three Phase 2 pruning calls from a
Transit + BikeShare evaluation. The state counts were `[1, 25, 25]` (51 total).
Both the production and test-reference paths still used the original code in
this run. Across 31 samples after 5 warmups, with 64 replays per captured call,
the medians were 8.862 µs and 8.795 µs per call respectively. Treat that
difference as measurement noise, not an optimization result.

After the change, a release test binary captured six calls across Transit-only
and Transit + BikeShare Phase 2 evaluations. It compared the original reference
and optimized function on the same captured pools in the same binary:

| Measure | Original reference | Optimized |
| --- | ---: | ---: |
| Calls per pass | 6 | 6 |
| Input states | 100 | 100 |
| Output states | 100 | 100 |
| `PathMetrics::from_movements()` calls | 198 | 100 |
| Median time per call | 12.7619 µs | 12.7628 µs |

The per-call input sizes were `[1, 1, 24, 24, 25, 25]`. There were 2,304
`same_capability()` comparisons, no `no_worse()` comparisons, and the largest
frontier before truncation contained 25 states. Comparison counts and outputs
matched between implementations. The metric count is the original instrumented
reference count versus one computation per optimized input state.

This deterministic captured workload cut metric construction by 49.5%, but
showed no measurable pruning-time change: across 31 samples after 5 warmups,
with 64 replays per captured call, the paired medians differed by 0.9 ns per
call (about 0.007%). No allocation profiler was available, so allocation
counts are not reported. The benchmark clones pools before starting each
timed sample and does not add per-state tracing to the timed loop.

Phase D.5 removed the capture and profiling harness because it collected state
pools through process-wide mutable test state. The measurements and workload
details above are retained as historical results; the old command no longer
runs, and no current harness replays those exact six pools. Phase D.6 will own
future performance measurements.

## End-to-end status

No full P2 latency claim is made. During validation the host reported 9,668 MB
of 10,240 MB swap in use, so overall P2 latency would not be reliable under
these conditions. Full P2 benchmarking is deferred until memory and Valhalla
latency are stable.
