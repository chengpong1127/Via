# Phase 2 strategy comparison

## Setup

- Commit: `a27bbcda07e5fa18af7aebb6edc9d4fb2b37d901`
- Experiment time: `2026-09-26T18:23:48+00:00`
- Transit network revision: `routing-fixture-static`
- Schedule revision: `routing-fixture-schedule`
- Timing mode: deterministic fixture schedules (`--service-conditions fixture`); no TDX requests.
- Street routing: the CLI's built-in `StraightLineStreetRouter`; local Valhalla was unavailable.
- Corpus: `6` fixture ODs at `2026-08-31T10:00:00+08:00`; 5 paired repeats and 5 standalone repeats after one discarded warmup.
- Limits: K=20/50/100; max transfers=4; max departures/query=3; temporal expansion limit=2,000; static search expansion limit=20,000; candidates=16.
- Corpus limitation: the fixture has no THSR→Metro transfer, so that case is a deliberate no-route control. The checked-in Taipei artifact was rejected by the current loader because it uses the older `services` JSON schema. No current-format real-network artifact was available.
- Live TDX was skipped because `TDX_CLIENT_ID` and `TDX_CLIENT_SECRET` are unset. The report therefore measures fixture schedule quality, not live service quality.

## Paired quality results

Each paired row that executed Phase 2 ran Compare mode, so both algorithms used the same Phase 1 corridors and one loaded TimingSnapshot. Rows with `phase2_executed=false` had no retained transit corridor, so Phase 2 did not run or load a snapshot. A negative arrival delta means Union arrived earlier. Winner-agreement and arrival-delta statistics include only requests where both strategies found a route; success and no-route counts use every request.

| K | Requests | Both no route | Fixed success | Union success | Same exact winner | Same structural winner | Union recombination | Fixed none / Union route | Union none / Fixed route | Arrival delta mean / median / p95 / min..max | Recombination gain mean / median / p95 / max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 20 | 30 | 5 | 83.3% | 83.3% | 100.0% | 100.0% | 0.0% | 0 (0.0%) | 0 (0.0%) | 0.00 s / 0.00 s / 0.00 s / 0.00 s..0.00 s | n/a / n/a / n/a / n/a |
| 50 | 30 | 5 | 83.3% | 83.3% | 100.0% | 100.0% | 0.0% | 0 (0.0%) | 0 (0.0%) | 0.00 s / 0.00 s / 0.00 s / 0.00 s..0.00 s | n/a / n/a / n/a / n/a |
| 100 | 30 | 5 | 83.3% | 83.3% | 100.0% | 100.0% | 0.0% | 0 (0.0%) | 0 (0.0%) | 0.00 s / 0.00 s / 0.00 s / 0.00 s..0.00 s | n/a / n/a / n/a / n/a |

## Standalone Phase 2 performance

Each strategy ran in its own process. Union and Fixed order alternated by case, K, and repetition. Phase 2 sample counts exclude requests with no retained transit corridor; CLI wall time includes every request, process startup, and fixture setup.

| K | Strategy | Phase 2 samples | Phase 2 median | Phase 2 p95 | Departure lookups median | States median | Candidates median | CLI wall median / p95 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 20 | Fixed | 25 | 0.043 ms | 0.079 ms | 10 | 14 | 1 | 1.752 / 2.330 ms |
| 20 | Union | 25 | 0.110 ms | 0.147 ms | 6 | 6 | 1 | 1.772 / 2.758 ms |
| 50 | Fixed | 25 | 0.043 ms | 0.101 ms | 10 | 14 | 1 | 1.578 / 9.500 ms |
| 50 | Union | 25 | 0.108 ms | 0.154 ms | 6 | 6 | 1 | 1.711 / 3.946 ms |
| 100 | Fixed | 25 | 0.045 ms | 0.086 ms | 10 | 14 | 1 | 1.729 / 2.086 ms |
| 100 | Union | 25 | 0.110 ms | 0.160 ms | 6 | 6 | 1 | 1.786 / 4.041 ms |

## Disagreement cases

No paired disagreement was observed in this corpus.
## Interpretation and limits

Across K=20/50/100, both strategies returned a Phase 2 route on 25 of 30 paired runs; the remaining five were the THSR→Metro no-corridor control. On all 25 route-producing pairs, Fixed and Union selected the same exact and structural winner with the same arrival time. Union recombined zero routes and rescued zero requests where Fixed had no route. Increasing K changed none of these quality results.

Fixed Phase 2 median latency was 43–45 μs, compared with 108–110 μs for Union; p95 was 79–101 μs for Fixed and 147–160 μs for Union. Fixed made more departure lookups and evaluated more corridor states in this fixture, but still used less measured Phase 2 time. End-to-end CLI medians overlapped at roughly 1.6–1.8 ms, while the CLI p95s had process-level outliers, so the Phase 2 CPU difference did not produce a reliable total-route latency difference here.

This small fixture shows no measured quality benefit for Union and lower Phase 2 cost for Fixed. It does not show how often Union recombination helps on production corridors: five ODs produced transit candidates, there was one departure time, and the only no-route case had no Phase 1 corridor at all. The K values also produced no observed sensitivity. The results are too narrow to change the production default based on quality.

A production decision needs a current-format GTFS-built artifact and a working local Valhalla instance. Then rerun this corpus with 30–50 meaningful ODs and multiple departure times, and run paired Compare with live TDX when credentials are available. Keep CPU-only Phase 2 latency separate from snapshot-loading and route-process wall time.

Raw parsed observations: [`phase2-comparison-raw.json`](phase2-comparison-raw.json). Re-run with `python3 server/benchmarks/phase2.py --binary server/target/release/via-route --report docs/phase2-comparison.md --raw docs/phase2-comparison-raw.json --repetitions 5`.
