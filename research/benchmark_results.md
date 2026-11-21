# Benchmark results

## Phase 1 corridor search — `corridor_bench` (100 Taipei OD scenarios, 5 iterations, median per query)

Command (from `server/`): `cargo run --release --example corridor_bench -- data/build/20261120T163303Z/.via-static.json out.jsonl 5`
Machine: Apple Silicon Mac, single-threaded, quiet. Artifact 2026-11-20 GTFS (106 823 stops, 6 766 patterns, 1 066 275 transfers).

| Variant | p50 ms | p95 ms | p99 ms | max ms | mean ms | max RSS | Corridors vs baseline |
| --- | --- | --- | --- | --- | --- | --- | --- |
| baseline `4769cf7` | 418.4 | 989.4 | 1118.5 | 1689.3 | 462.0 | 1.43 GB | — |
| EXP-001 dense indices | 95.3 | 214.8 | 271.6 | 383.6 | 102.6 | 1.26 GB | identical (100/100) |
| EXP-001 after review cleanups `perf/dense-corridor-search` | 85.0 | 189.1 | 233.4 | 374.4 | 95.2 | — | identical (100/100) |

Per-scenario speedup median 4.49×, max 5.67×. Two scenarios with no usable access stops go from ~0.001 ms to ~0.28 ms
(fixed allocation of per-stop label arrays).
Baseline states expanded per query: median ≈146 k, max ≈657 k (2026-11-02 artifact).

## EXP-004 static headway waits — `corridor_bench` with departure time (100 scenarios, 3 iterations)

Artifact: 2026-11-20 GTFS plus TDX Bus/Schedule headways fetched 2026-11-21 (`examples/attach_headways.rs`).
Command: `corridor_bench <artifact> out.jsonl 3 <RFC3339 departure>`. Cap = `MAX_EXPECTED_WAIT_SECONDS`.

| Variant | Mon 08:30 p50 / p95 / p99 ms | Wed 12:00 p50 / p99 ms | Sat 10:30 p50 / p99 ms | no-service corridors (Mon / Wed / Sat) |
| --- | --- | --- | --- | --- |
| base (no headways) | 91 / 203 / 247 | 91 / 245 | — | 496 / 767 / 1,024 |
| cap 0 (no-service only) | 74 / 179 / 261 | 77 / 273 | 76 / 292 | 9 / 13 / 25 |
| cap 300 s | 67 / 182 / 261 | 70 / 268 | 68 / 285 | 21 / 53 / 78 |
| cap 900 s | 63 / 219 / 302 | 69 / 300 | — | 41 / 114 / 184 |

Live cap 900 vs base, two Saturday runs (40 paired plans): 5 earlier, 4 later, 31 same; no-service only (run 2): 1 / 1 / 18; see `experiments/EXP-004-headway-waits.md`.

## EXP-005 bus parent routes — live, base vs base+parents (20 scenarios, Sat 2026-11-21 14:06)

2 earlier (−13.5, −39 min), 0 later, 18 same; walking only 4 / 4; median planning
latency 494 → 442 ms. See `experiments/EXP-005-bus-parent-routes.md`.
