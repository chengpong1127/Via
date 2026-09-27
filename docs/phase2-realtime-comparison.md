# Taipei Phase 2 Fixed vs Union comparison

**Capture date:** 2026-09-27, 12:11–13:52 Asia/Taipei

**Result:** retain Union provisionally for transit quality; do not treat this 12-OD sample as a network-wide decision.

## Finding

Of 108 paired Compare runs (12 real-stop ODs × K=20/50/100 × three rounds), 50 snapshots contained at least one live `RealtimePrediction` or `DelayAdjustedSchedule` value and had no failed timing values. Schedule-only and empty snapshots were excluded from realtime quality totals. Fixed returned a Phase 2 route in 30 of those 50 observations; Union returned one in 35. There were five Union-only results and no Fixed-only results.

All five Union-only results came from the same cross-Taipei Bus OD, 酒泉街 (TPE11870) → 捷運大安站 (TPE11896): Union recovered it in all three live K=50 rounds and two of three live K=100 rounds. Fixed found no Phase 2 route in those observations. The Union winner was outside the retained Top-K corridors structurally, so these were recombinations. K=20 did not find the route under either strategy.

When both strategies returned a route, their best Phase 2 candidate signatures matched in 21 of 30 observations. Union arrived earlier in the other nine by 12–152 seconds; Fixed was never earlier. These nine are repeated Route 41 observations on the adjacent and short corridor ODs, with a different access/boarding anchor on the same transit pattern. The results are correlated by OD and should not be read as nine independent route improvements.

Given the repeated route recovery and absence of a Fixed-only result, these measurements do not support calling the strategies equivalent. Keep Union for the tested transit flows. The recovery is concentrated on one OD, however, so use this as a local decision and widen the OD sample before treating it as a Taipei-wide conclusion.

## Route availability by K

Only qualifying realtime snapshots are counted here. Each K had 36 requested observations (12 ODs × 3 rounds); the sample counts below are the subset with live timing values.

| K | Live observations | Fixed route | Union route | Union only | Fixed only | Neither |
|---:|---:|---:|---:|---:|---:|---:|
| 20 | 14 | 8 (57.1%) | 8 (57.1%) | 0 | 0 | 6 |
| 50 | 17 | 12 (70.6%) | 15 (88.2%) | 3 | 0 | 2 |
| 100 | 19 | 10 (52.6%) | 12 (63.2%) | 2 | 0 | 7 |
| **All K** | **50** | **30 (60.0%)** | **35 (70.0%)** | **5** | **0** | **15** |

“Union only” means Fixed returned no route while Union returned one. The five observations are five repeats of the same OD at K=50/100, not five distinct corridors.

## Best-route differences and recombination

Across the 30 live observations where both strategies returned candidates, the best signature was the same in 21. For all 30, the raw comparison field `best_arrival_delta_seconds` is Union arrival minus Fixed arrival; the normalized differences in this report use Fixed arrival minus Union arrival, so a positive value means Union arrived earlier.

| K | Both returned a route | Same best signature | Union-only routes | Union winners retained exactly / by structural substitution / recombined |
|---:|---:|---:|---:|---:|
| 20 | 8 | 8 | 0 | 7 / 1 / 0 |
| 50 | 12 | 7 | 3 | 5 / 2 / 8 |
| 100 | 10 | 6 | 2 | 4 / 2 / 6 |
| **All K** | **30** | **21** | **5** | **16 / 5 / 14** |

The recombined winners were all on the Route 41 ODs. For the cross-Taipei OD, Union produced a direct Route 41 candidate even though the retained static Top-K contained longer multi-leg corridors. In the both-route Route 41 cases, Union selected a different access/boarding anchor on that pattern and arrived 12–152 seconds earlier. The two largest gains were at least 120 seconds; none reached five minutes. No paired observation had Union arriving later.

## Phase 2 search cost

These are the planner’s per-strategy Phase 2 elapsed microseconds from the shared-snapshot Compare run. They are not process CPU-cycle measurements and exclude TDX snapshot-loading time. The p95 uses nearest rank; because each K has only 14–19 qualifying observations, p95 equals the largest observation in that group.

| K | Fixed median / p95 | Union median / p95 | Median departure lookups Fixed / Union | Median states Fixed / Union |
|---:|---:|---:|---:|---:|
| 20 | 0.170 / 0.754 ms | 0.474 / 50.775 ms | 20 / 9 | 32 / 24.5 |
| 50 | 0.310 / 1.390 ms | 1.324 / 119.189 ms | 77 / 33 | 119 / 33 |
| 100 | 0.783 / 13.880 ms | 2.208 / 137.504 ms | 130 / 45 | 157 / 42 |

Fixed had lower median and tail latency at every K. Union expanded fewer states at the median, but its live Metro→TRA observations issued many more departure lookups and account for the largest Union timings: 1,082–2,336 lookups across the three K settings. This is the main performance risk to investigate if Union remains enabled.

## Live-data coverage and method

- The runner invoked `via-route --depart-now --service-conditions live --phase2-strategy compare` against the local Taipei artifact and Valhalla 3.9.0. A 30-second pause separated requests. Compare loaded one request-local TimingSnapshot shared by Fixed and Union.
- The 12 ODs cover adjacent, short, and cross-Taipei Bus trips; Bus↔Metro and a Metro transfer; TRA↔Metro; and THSR↔Metro. The coordinates map to stops in the local GTFS-derived network. Each K was run for all 12 ODs in each of three rounds.
- Fifty snapshots qualified: 41 contained Bus `RealtimePrediction` rows and 9 contained rail `DelayAdjustedSchedule` rows. Across those snapshots the raw basis counts total 124 `RealtimePrediction` and 115 `DelayAdjustedSchedule` values. The other 58 requests had no qualifying live values: 40 had a TimingSnapshot with schedule-only or empty values, and 18 had no TimingSnapshot to qualify. No fixture or schedule value was counted as realtime. The complete capture had zero failed timing values and zero rate-limit outcomes.
- The supplied local GTFS archive had SHA-256 `a7a84ed30bd949ab912a1b32696c07a0032f99165bb90d19c5bd81a5229916fa`. Reproducible preparation cleared 90 unsupported `direction_id=2` values and normalized 108 `HH:MM` times to `HH:MM:00`; no other transit fields changed. The prepared archive SHA-256 is `bae85f6801dba87207eb0b9d9357175b6c938db7aac77438ef9e366dd0dc0378`.
- The schema-7 artifact was built from that prepared archive using local Valhalla 3.9.0. It contains 98,197 stops, 6,826 routes, 6,171 patterns, 86,214 trips, and 950,193 transfer edges. Static network revision: `sha256:3a40820924cfd90e010159a50b4480815d2ce00d556bb32abad055fbb69b68d1`; schedule revision: `sha256:74a2104ef0125a8ecf94a0f530a689178c136dd7c5b4c1825ce2a9b47dfb6bb5`.
- A live Bus response contained `RouteUID` but no `SubRouteUID`; the previous adapter discarded that estimate. Commit `86faa8313174f8dcb4c1026363496fdc25dd747a` adds a `RouteUID` fallback when `SubRouteUID` is absent, constrained to the requested service IDs. The focused parser test and release binary build passed.

## YouBike implications

No BikeShare mode was selected in this comparison. The GTFS-derived artifact has zero BikeShare stations and zero BikeShare access links, and this run did not request a BikeShare availability snapshot. It therefore cannot measure the integration effort or route-quality effect of YouBike.

Keep the current boundary for a later BikeShare evaluation: load one request-local availability snapshot and let BikeShare replace an existing adjacent boarding-point movement. The TransitPlanner comparison does not establish a need for BikeShare to add transit connectivity, access/egress alternatives, or direct BikeShare-only routes. Test those path variants after a static station catalog is selected.

## Reproduction and artifacts

The raw JSON contains all 108 paired rows, per-row live basis counts, Phase 1 and Phase 2 metrics, best signatures, retained-corridor evidence for disagreements, and artifact/corpus hashes. It records no credential values.

- Raw capture: [`phase2-realtime-comparison-raw.json`](phase2-realtime-comparison-raw.json)
- OD corpus: [`phase2-realtime-taipei.json`](../server/fixtures/routing/phase2-realtime-taipei.json)
- Capture runner: [`phase2_realtime.py`](../server/benchmarks/phase2_realtime.py)
- GTFS preparation script: [`prepare_realtime_gtfs.py`](../server/benchmarks/prepare_realtime_gtfs.py)
- Source commit used by the release binary: `86faa8313174f8dcb4c1026363496fdc25dd747a`

To repeat the full capture with live TDX credentials configured in `server/.env` and the local artifact/binary available:

```bash
python3 server/benchmarks/phase2_realtime.py \
  --binary server/target/release/via-route \
  --rounds 3 \
  --interval-seconds 30 \
  --skip-standalone
```
