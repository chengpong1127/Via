# Phase D.3: Phase 2 profiling

## Scope and repeatability

Measured branch `p2-optimization`, commit `5b00e6e`, using the existing ignored `phase2_corridor_representative_benchmark` against the latest `buffer_unordered()` implementation. It uses the fixed 2026-10-01 10:00 +08:00 Taipei Main Station–Xinyi request and local 20261001T173300Z static artifact, static timetable timing, fixture bike availability, and a running Valhalla service. Phase 2 state concurrency is 4, Valhalla concurrency is 8, and deadlines are disabled. Each corridor-concurrency setting has one warmup and three measurements in rotated order. Setup is excluded from the reported `session.evaluate()` wall time; reconstruction and ranking run separately after the last sample. The aarch64 host reported eight CPUs, 7.8 GiB RAM and approximately 5 GiB swap used; the provider and host were not isolated. Three samples do not characterize tail latency.

```sh
cd server
CARGO_BUILD_JOBS=1 cargo test --release --locked --lib phase2_corridor_representative_benchmark -- --ignored --nocapture
CARGO_BUILD_JOBS=1 VIA_CHROME_TRACE=/tmp/via-d3-trace.json cargo test --release --locked --lib phase2_corridor_representative_benchmark -- --ignored --nocapture
```

The [untraced](benchmarks/phase2-d3-baseline.txt) and [traced](benchmarks/phase2-d3-traced.txt) logs contain all samples, counts, candidate digests and Top-3 digests. The trace was written to `/tmp/via-d3-trace.json` and is not retained in the repository. Both runs passed, with zero provider failures or timeouts and matching candidate and final Top-3 key digests within each workload. Top-3 contained one Transit Only and three mixed options; matching keys do not establish complete option equality.

## Baseline and tracing overhead

Medians in milliseconds, three measured samples per cell; percent changes compare separate runs, so include host/provider variation as well as tracing overhead.

| Workload | Corridor concurrency | Untraced | Traced | Difference |
| --- | ---: | ---: | ---: | ---: |
| Transit | 1 | 1,027.9 | 1,111.4 | +8% |
| Transit | 2 | 906.9 | 981.0 | +8% |
| Transit | 4 | 818.9 | 855.1 | +4% |
| Transit | 8 | 772.0 | 853.8 | +11% |
| Transit + BikeShare | 1 | 3,497.7 | 4,026.7 | +15% |
| Transit + BikeShare | 2 | 2,666.3 | 3,294.3 | +24% |
| Transit + BikeShare | 4 | 1,830.3 | 2,578.7 | +41% |
| Transit + BikeShare | 8 | 1,486.0 | 2,099.8 | +41% |

Each untraced Transit sample expanded 4,175 states, generated 5,896 successors, returned 1,139 candidates and made 51 Matrix HTTP calls. Each mixed sample expanded 10,242 states, generated 101,132 successors, returned 242 candidates and made 279 Matrix calls. These are not paired pre- and post-pruning counts: successor counts aggregate across all edges and final candidates also go through finalization and global Pareto filtering. Counts before and after each pruning operation were not captured.

The union of existing `Valhalla HTTP` async spans intersected with each traced `Corridor Evaluation` interval covers 21–52% of Transit and 94–96% of mixed evaluation wall time across measured settings. This means at least one HTTP operation was in flight during those intervals; CPU work and multiple requests can overlap. It is not time spent blocked, exclusive I/O time, or a CPU estimate. The HTTP span includes response processing. The trace includes `Route Cache` spans but does not directly separate ready cache hits from time waiting for in-flight entries. At concurrency 8, mixed untraced summed provider semaphore waits were 33–69 ms per sample, versus at most 0.024 ms at 1–4; sums overlap and are not wall time. Mixed ready/in-flight cache hits at 8 ranged 448–454 / 121–127; at 1 they were 571 / 4. Matrix/cache waiting versus active CPU remains unquantified.

## Exploratory stack sampling

Samply 0.13.1 could not start: `perf_event_paranoid=2`, and `/proc/sys` is mounted read-only. A fallback used LLDB to attach to the benchmark process, interrupt it approximately every 100 ms, collect all thread stacks, and resume it. The symbolized release test binary was built with the release optimization settings plus line tables, no symbol stripping and frame pointers. The [sampler script](via_lldb_sample.py), [sampled run](benchmarks/phase2-d3-sampled.txt), and [raw snapshots](benchmarks/phase2-d3-lldb-samples.jsonl.gz) are included. The script expects the sampled process's stdout at `/tmp/via-d3-sampled.txt` and writes snapshots to `/tmp/via-d3-lldb-samples.jsonl`. The debugger was attached after the first capture. Snapshot labels are the most recently printed benchmark line, so they indicate workload but not the precise current concurrency or stage. For this run the binary was built with a `RUSTC_WORKSPACE_WRAPPER` invoking rustc with `-C strip=none -C debuginfo=1 -C force-frame-pointers=yes`; launch it directly with `phase2_corridor_representative_benchmark --ignored --nocapture`, then run `lldb -b -o 'command script import /path/to/via_lldb_sample.py' -o 'samplevia PID' -o quit`.

This is a **stop-the-world wall-clock stack sampler**, not an on-CPU profiler. It is biased by debugger pause, stack-unwinding cost, async scheduling, nonuniform sampling and compiler inlining. It cannot establish CPU percentages, call counts or allocation volumes, and absence of a symbol is not evidence of no work. Out of 185 Transit snapshots, 97 contained an evaluator stack, 93 of those contained `pareto_candidates`, and 67 included journey identity code. Out of 400 mixed snapshots, 47 contained an evaluator stack, 18 included `prune_states`, 12 `same_capability`, 11 `pareto_candidates`, and 5 `bike_next_states`. Categories overlap. These are stack occurrences, not independent calls or a statistically representative fraction of CPU. `PathMetrics::from_movements` and `EvaluationState::clone` were not separately resolved in these snapshots; generic clone frames appeared in 26 Transit and 7 mixed evaluator stacks. No allocation profiler was available, so major allocation hotspots remain unmeasured.

The stops significantly perturbed execution. For example, at corridor concurrency 1, sampled measured rounds 1 and 2 were 2,135 and 2,375 ms for Transit, versus 960 and 1,028 ms untraced; mixed was 7,051 and 6,847 ms versus 3,488 and 3,498 ms. The symbolized build itself also differs from the baseline, and provider/host variation contributes. Sampled execution had no Matrix timeouts and produced matching candidate and Top-3 key digests. These comparisons cannot isolate debugger overhead precisely.

## Candidates and suggested order

This is an exploratory priority order, not a definitive CPU or allocation ranking:

1. **Final candidate Pareto pass (especially Transit Only):** `pareto_candidates` was the most frequent named evaluator frame in the Transit snapshots. It repeatedly builds journey identities while comparing against the growing frontier, and builds canonical signatures for sorting. Cache exact identities and signatures per candidate, then measure again. Expected benefit is potentially high for Transit Only; correctness risk is medium because identity, hydration, timing quality and stable ordering govern deduplication.
2. **State pruning and metric recomputation (especially mixed):** `prune_states` computes metrics for sorting and each candidate, then recomputes known metrics inside frontier comparisons. `PathMetrics::from_movements` traverses histories and clones trip IDs and evidence, while `same_timing_bases` creates temporary vectors. Consider caching metrics per state and comparing normalized timing bases without allocation. Benefit could be high for large frontiers; correctness risk is medium because every movement, including extended rentals and timing evidence, must update metrics exactly.
3. **Movement histories and BikeShare expansion:** `after_movement` clones the whole state; `after_bike_share` clones the prior history into a prefix and then clones that prefix again, while retaining rental metadata. `bike_next_states` creates one successor for each feasible realized bike movement. Consider shared immutable histories or a more compact rental representation. Benefit may be high for the mixed workload's much larger successor count; correctness risk is medium to high around continuous rentals, ownership and equality.
4. **Capability comparisons:** `same_capability` scans histories for hydration and compares movement histories and active rental prefixes in frontier comparisons. Cache the hydration property or a collision-safe identity, preserving full equality semantics. Potential benefit depends on measured frontier sizes; correctness risk is medium because overly broad equivalence can prune valid journeys.


Do not implement any of these solely on these counts. Measure CPU samples, allocation call stacks and counts, pruning input/output distributions and practical call counts first; preserve journey identities, timing evidence, rental reuse, feasibility and ordering in regression comparisons.
