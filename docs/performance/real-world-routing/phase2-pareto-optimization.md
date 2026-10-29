# Phase D.4a: final candidate Pareto pruning

The final pass now constructs each candidate's exact journey identity, canonical signature and hydration flag once. The same stable sort keys and frontier iteration order are used, identity comparison remains exact, and dominance still invokes the same `PathMetrics::dominates` only when the left journey has no surface movements. The cached signature is used only for ordering; it does not replace identity equality. No search, state expansion, provider or reconstruction behavior changed.

## Measurements

The existing representative benchmark was run in release mode against commit `5b00e6e` and this change, with the 2026-10-01 static artifact, fixed Taipei Main Station–Xinyi input, static schedule and fixture bike availability, and local Valhalla. State concurrency was 4, provider concurrency 8, deadlines disabled. The isolated measurement captures the first pre-Pareto pool per workload, clones it outside each timed call, warms up once, and records 15 repetitions. The original pass is copied as a test-only reference. In the before/after runs in the table, its comparison counts are collected separately from timed runs. The baseline and optimized runs captured pools in separate sessions; the paired run below also compares and times both passes on the same captured pool. Measurements are on a non-isolated aarch64 host and should not be treated as production latency.

| Pool | Before → after candidates | Peak frontier | Original identity / signature constructions | Optimized identity / signature constructions | Dominance comparisons | Pareto before median (range), ms | Pareto after median (range), ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Transit | 1,139 → 1,139 | 1,139 | 1,297,321 / 1,139 | 1,139 / 1,139 | 1,296,182 | 605.3 (588.2–636.6) | 5.1 (5.0–7.4) |
| Transit + BikeShare | 1,434 → 242 | 242 | 329,204 / 1,434 | 1,434 / 1,434 | 191,854 | 59.2 (58.4–61.2) | 2.8 (2.8–3.0) |

In a separate [paired run](benchmarks/phase2-pareto-paired.txt), both passes used clones of the exact same captured pool, with one warmup and 15 measurements each. The original reference includes local counters in its timed loop, while the optimized pass does not, so these timings include that instrumentation difference. Transit median (range) was 649.7 (633.6–1,020.9) ms for the reference and 5.65 (5.57–8.85) ms optimized; mixed was 66.51 (66.26–72.35) ms and 2.93 (2.89–3.05) ms respectively. The independently established uninstrumented original baseline above supports the magnitude of the difference.

Original counts are measured by the reference pass. Optimized identity and signature counts are derived from the implementation's single construction per input, and dominance comparison counts and frontier sizes are from the reference, whose decisions are structurally compared to the optimized pass. They are not separate runtime counters for the optimized pass. Allocations were not measured: a reliable allocation profiler was unavailable. Caching adds metadata and holds identities and signatures through the pass, so reduced identity construction does not by itself quantify peak allocation or memory.

The raw [before](benchmarks/phase2-pareto-before.txt) and [after](benchmarks/phase2-pareto-after.txt) logs include all samples. Capture mode adds benchmarking and reference checks inside the first warmup evaluation, so those warmups must not be used as end-to-end measurements. Other rounds use the ordinary evaluation path apart from a test-only disabled-capture check. Use:

```sh
cd server
CARGO_BUILD_JOBS=1 VIA_PARETO_BENCH=1 VIA_PARETO_BENCH_SHORT=1 cargo test --release --locked --lib phase2_corridor_representative_benchmark -- --ignored --nocapture
```

## End-to-end Phase 2

Medians of three measurements after one warmup, in milliseconds. The before run had capture enabled only on the warmup; the [after run](benchmarks/phase2-pareto-after-uninstrumented.txt) had capture disabled. The two sequential runs were not isolated or interleaved. Swap use rose from approximately 5 GiB during D.3 to 7.3 GiB during the after sweep, and provider/host load was uncontrolled. The raw logs contain the complete distributions. Both runs had matching candidate digests and no provider timeouts; exact output comparison was also performed in the capture run described below.

| Workload | Corridor concurrency | Before median | After median | Change |
| --- | ---: | ---: | ---: | ---: |
| Transit | 1 | 987.8 | 773.8 | -22% |
| Transit | 2 | 884.8 | 583.4 | -34% |
| Transit | 4 | 792.7 | 483.1 | -39% |
| Transit | 8 | 725.6 | 254.3 | -65% |
| Transit + BikeShare | 1 | 3,672.9 | 6,631.5 | +81% |
| Transit + BikeShare | 2 | 2,792.9 | 5,693.4 | +104% |
| Transit + BikeShare | 4 | 1,970.7 | 4,488.9 | +128% |
| Transit + BikeShare | 8 | 1,437.9 | 3,225.2 | +124% |

Transit improved in these runs, consistent with the isolated result, though the size of the end-to-end improvement is confounded by host and provider variation. Mixed end-to-end latency regressed in both after sweeps despite the isolated pass becoming faster and identical state, successor, request and output counts. The cause is unmeasured; increased swap and provider load are plausible but not established. A controlled, interleaved repeat is needed before claiming any end-to-end mixed benefit or attributing the regression to the change.

## Correctness

The opt-in capture runs the original and optimized pass on the same captured pool and compares each ordered identity, full `RealizedJourney`, and `PathMetrics` structurally. For both workloads it reconstructs and ranks the original and optimized outputs and compares the complete ranked result as JSON values and serialized strings, including all Top-3 fields. The Transit pool yields one ranked option and the mixed pool three. The focused regression exercises duplicate identities, equal sorting keys, differing histories, transit and surface movements, timing evidence, fallback reliability and dominated candidates across input rotations. The reference implementation is deliberately retained in test support. The representative checks depend on the fixed local artifact and live Valhalla and are ignored by default.
