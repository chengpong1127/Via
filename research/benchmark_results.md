# Benchmark results

## Phase 1 corridor search — `corridor_bench` (100 Taipei OD scenarios, 5 iterations, median per query)

Command (from `server/`): `cargo run --release --example corridor_bench -- data/build/20261120T163303Z/.via-static.json out.jsonl 5`
Machine: Apple Silicon Mac, single-threaded, quiet. Artifact 2026-11-20 GTFS (106 823 stops, 6 766 patterns, 1 066 275 transfers).

| Variant | p50 ms | p95 ms | p99 ms | max ms | mean ms | max RSS | Corridors vs baseline |
| --- | --- | --- | --- | --- | --- | --- | --- |
| baseline `4769cf7` | 418.4 | 989.4 | 1118.5 | 1689.3 | 462.0 | 1.43 GB | — |
| EXP-001 dense indices | 95.3 | 214.8 | 271.6 | 383.6 | 102.6 | 1.26 GB | identical (100/100) |

Per-scenario speedup median 4.49×, max 5.67×. Two scenarios with no usable access stops go from ~0.001 ms to ~0.28 ms
(fixed allocation of per-stop label arrays).
Baseline states expanded per query: median ≈146 k, max ≈657 k (2026-11-02 artifact).
