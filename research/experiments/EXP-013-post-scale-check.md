# EXP-013: post-#49 check, replay of the 38 (+20) scenarios on the scale-290 artifact

Date: 2026-11-26 evening (Asia/Taipei). Zero TDX requests (replay). Question: with `bus_derived_segment_scale_percent = 290`
(main after #49, #42), do the B28 acceptance targets hold?

## Setup
- Artifact: the EXP-011 artifact `20261125T004225Z` (GTFS 2026-11-20, Bus routes, Bus/Schedule headways; scale 100) rescaled
  with `tools/rescale_bus_artifact.py ... 290`. It applies the same operation as `TransitNetwork::scale_bus_segment_estimates(290)`
  (Bus routes, non-`schedule` segments, ceil(s x 290 / 100)). A real `via-data build` is equivalent (the scaling runs before the
  walking-transfer and bike-access steps, which do not read segment seconds); the real build failed after 33 min with
  `StreetRouting(TemporarilyUnavailable)` (the Valhalla port-forward dropped), so the rescale was used. 161,273 of 209,107 Bus segments
  scaled; total Bus segment seconds x1.80 (the rest are `schedule` segments).
- Code: `origin/main` c9b1293 (est is always on; `noest` = provider bindings with the headway lists cleared, in `live_compare`
  on `research/exp-013-post-scale-check`). Variants `est` (main) and `noest` (est off, the old `base`).
- Replay: `tools/tdx_replay.py` over the three recordings (set A 09:07, set B 11:11, set A again 12:54), departure pinned to
  the original planning time, fresh/degraded 1e8 s. 58 plans x 2 variants in about 20 s each set.
  Google column: the same aligned Google passes as EXP-011 (`results/maps-google-aligned*.jsonl`); gap = Via duration minus
  Google door-to-door.
- Table: `tools/exp013_table.py` (rows in `results/exp013-replay-{a,b,c}-scale290.jsonl`).

## Result against the B28 targets (sets A+B, 38 scenarios)
| measure | scale 100 live | x2.5 replay | scale 290 replay | B28 target |
| --- | --- | --- | --- | --- |
| median gap to Google (min) | -4.8 | +0.8 | +0.8 | within +-3: met |
| plans Via over 10 min faster than Google | 10 | 3 | 3 | at most 4: met |
| est earlier than noest by >30 s | 11 | 5 | 3 | about 5: 3 (fewer) |
| plan later than noest by >30 s | 0 | 0 | 0 | none: met |
| A2 median actual/model, 11:00 and 12:30 snapshots | - | - | 0.96, 0.95 | 0.85-1.15: met |
All 58 plans (set A repeated at 12:54): median gap +2.6 min, mean +12.5, over-10-min-faster 6, est wins 3, none later.
Mean gap is dominated by plans that need lines the feed lacks (below).

est wins on scale 290: ntu_to_nangang -9 min (59 to 50), luzhou_to_bali -71 min (146 to 75), bali_to_taipei -8 min (111 to 103).
Set A repeated: 0 est wins (7 at scale 100). est is still never worse.

## Held-out ride-time check (`server/docs/calibration/bus_ride_time.py analyze` on the scale-290 artifact)
11:00 snapshot: median 0.96, p10 0.58, p90 1.26 (1,687 obs); 12:30: median 0.95, p10 0.59, p90 1.25 (1,330 obs).
Per city: Taipei 1.04-1.05, New Taipei 0.93-0.95, Taoyuan 0.86-0.87, Tainan 0.73. The 290 factor is about 4-5 percent high
overall (275 would centre it); the spread between legs (p10-p90 0.58-1.26) is far larger than the city spread.

## Where Via is still slower than Google (scale 290, est)
- Upstream gaps: Wenhu BR (banqiao_to_neihu +61, xiangshan_to_muzha +61, cathay_to_dazhi +51, xinzhuang_to_muzha +45 to +49,
  yongning_to_xihu +47 to +52 with a 92 min final walk), Circular Y (shulin_to_xinzhuang +16, walk 48 min).
- B30 coverage: banqiao_to_linkou +50 (Google rides the direct bus 948).
- Candidates for the next experiment (not feed gaps): xizhi_to_zoo +67 to +87 (Via: TRA to Songshan, walk 41 min, bus 292 for
  6 min, walk 62 min; Google: Bannan Line + bus 南環幹線 59); sanxia_to_banqiao +38 (walks 49 min to the TRA station; Google rides
  bus 910 from the stop next to the origin); luzhou_to_bali (146 min without est). All three end in walking legs of 40-60 min,
  so Phase 2 found no feasible bus ride. EXP-014 looks at why.
- Scale 290 vs x2.5: yongning_to_xihu 110 to 126 and xizhi_to_zoo 121 to 136 min get worse as the buses slow down.

## Decision
- #49 (scale 290) meets the B28 targets except the est-win count, which is a consequence, not a failure (fewer fake wins).
- Possible follow-up: percent 275 centres the held-out ratio (0.95 to about 1.0). Not worth a PR before the peak-hour runs.
- Missing-peak runs: the A2 scheduler (`tools/a2_snap.py`) is queued for 21:00 and tomorrow 08:00, 12:30, 15:00, 17:30, 18:30.

## Addendum 2026-11-26 21:45: evening ride-time ratio on the scale-290 artifact
A2 snapshots at 21:00 (default key) and 21:45 (experiment key, which works: 8 GETs + token), `results/exp012-a2-2100.jsonl.gz`, `exp012-a2-2145.jsonl.gz`:
5,917 observations, median actual/model 0.92 (p10 0.64, p90 1.17); hour 20: 0.96 (n=675), hour 21: 0.92 (n=5,242); Taipei 0.97, New Taipei 0.85.
So evening buses run about 5 percent faster than midday (0.95-0.96), well inside the 15 percent band; peak hours are still unmeasured.
The A2 scheduler now uses the experiment key (`A2_ENV`, `A2_SLEEP=6`): snapshots at 08:00, 08:30, 12:30, 15:00, 17:30, 18:30 on 2026-11-27, 9 requests each.
TDX request log for this evening: 2 snapshots = 18 requests (9 on the default key, 9 on the experiment key).
