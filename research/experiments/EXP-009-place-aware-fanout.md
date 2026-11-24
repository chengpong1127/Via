# EXP-009: Place-aware fan-out of walking transfers

Branch: `research/exp-008-common-lines` (prototype code: `examples/fanout_audit.rs`,
`examples/transfer_replay.rs`, `PLACE_FANOUT` in `src/data/static_transfers.rs`, edge injection in
`examples/corridor_bench.rs`). Status: in progress (audit done, real-edge replay running).
Date: 2026-11-24

## Problem

`via-data build` keeps the 12 shortest walking transfers per stop
(`preprocessing.transfers.max_outgoing = 12`). Taipei bus GTFS gives each route its own stop ID at
one physical stop (B12), so the 12 shortest targets are often copies of one place. Real targets one
block further away (a Metro station, another bus place) lose their edge.

## Audit (2026-11-24, `fanout_audit`, artifact research-20261124, 106,823 used stops)

| | value |
| --- | --- |
| stops at the 12 cap | 74,225 (69.5 %) |
| mean edges per stop / distinct target places per stop | 9.98 / 2.61 |
| edges whose target place is already reached by another edge | 73.9 % |
| places within 350 m crow-fly with no transfer edge | 248,234 of 385,829 (64.3 %) |
| stops missing a Metro/Rail/THSR place within 350 m | 10,011 (9.4 %) |
| same, 500 m | 16,759 (15.7 %) |

Caveat: crow-fly distance is not walkability. The missing places include rivers, rail and
highway barriers, and anything beyond the 480 s / 800 m limits. The real number removed by the
fan-out cap is `removed_by_fanout` of the build report, which the replay prints.

## Upper-bound corridor test (crow-fly edges, 100 scenarios)

`VIA_FANOUT_PLACES=12 corridor_bench`: for each stop with fewer than 12 reached places, add an
edge to the nearest stop of each unreached place within 400 m crow-fly (x1.3 detour, 1.4 m/s),
until 12 places. Not validated against the street network, so it is an upper bound.

- Best retained static cost: **better by more than 30 s in 31/97 scenarios** (best: -423 s),
  worse by more than 30 s in 12, within 30 s in 54. Three random scenarios had no corridors in both.
- Retained corridor set differs in 96/97 scenarios (mean 23.1 -> 23.2 corridors, 12.4 in common).
- States expanded 170k -> 180k. Latency not measurable (load average 40-250, swap in use).

## Real-edge replay (running)

`transfer_replay` re-runs the matrix stage on the artifact with the retention rule
"first the nearest stop of each place, then fill by duration" (`VIA_PLACE_FANOUT=1`). It reuses
the same Valhalla matrix queries as production, so the change costs no extra Valhalla load.
The edges are written to a file that `corridor_bench` loads via `VIA_TRANSFER_EDGES`.
