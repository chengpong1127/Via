# EXP-008: Common-lines transit edges vs headway-estimated departures

Branch: `research/exp-008-common-lines` (off `origin/main` e1eefeb)
Status: live A/B done 2026-11-24 19:42-21:20; PRs pending
Date: 2026-11-23

## Problem

Taipei and New Taipei buses have no GTFS trips. Their only timing is the TDX N1
estimate, which gives the next bus per stop and pattern. If that bus leaves before
the access walk ends, the leg has no departure, and Phase 2 often falls back to a
long walk (EXP-005: 4 of 20 live scenarios walking only).

A corridor's transit edge already lists every *equivalent leg* (same mode, boarding
stop ID and alighting stop ID), so Phase 2 takes whichever of those lines comes
first. Taipei bus GTFS uses one stop ID per route at a physical stop (B12), so the
equivalent set is usually a single line.

## Hypotheses

- **place** (common lines): widen the equivalent set to every leg between stops of
  the same *place* (same stop name, <=100 m, as in EXP-002). This is the classic
  common-lines / attractive-set model of transit assignment (Chriqui & Robillard
  1975; Spiess & Florian 1989): a rider at a stop boards the first bus of any line
  that serves the trip. More lines in the set mean a better chance that one of
  them has a predicted bus after the walk ends. No timing is invented.
- **est** (B18, owner-approved 2026-11-23): after the last fresh TDX prediction at a
  stop, add departures every TDX Bus/Schedule headway for up to 2 h, labelled
  `FrequencyEstimate` / `Fallback`, for patterns with no GTFS trips or frequencies.
  The prototype is a port of `research/exp-006-headway-departures` onto `origin/main`.
- **place+est**: both.

## Implementation (prototype)

- `DenseTopology`: `place_by_stop` and `stops_by_place` (union-find over same-name stops <=100 m),
  `same_place`, `stops_in_place`; global toggle `PLACE_EQUIVALENCE` for the A/B.
- `corridors::search::equivalent_legs_for`: board at any stop of the board place, alight
  at the first stop of the alight place; legs keep their own stop IDs.
- Reconstruction: transit boarding and BikeShare pickup accept the current stop's
  place instead of the exact stop ID. Known shortcut: the <=100 m walk between
  co-located stops is not timed or drawn.
- `providers::timing::bus`: `estimated_departures` (toggle `HEADWAY_ESTIMATES`),
  headways attached to `BusPatternBinding.headway_seconds_by_period` by
  `examples/attach_headways.rs` from the 2026-11-21 Bus/Schedule snapshot.
- Harness: `examples/live_compare.rs` with variants `base,place,est,place+est` in one
  process, one shared realtime cache, order rotated per scenario, 60 s per TDX request.

## Offline result (2026-11-24 00:25, `corridor_bench`, 100 scenarios, 2026-11-20 artifact)

Command (from `server/`): `VIA_PLACE=0|1 corridor_bench data/build/20261120T163303Z/.via-static.json out.jsonl 3`

| | transit edges | legs / edge | routes / edge | edges with >1 route | first-ride routes | bus board stops / query (mean, p95) |
| --- | --- | --- | --- | --- | --- | --- |
| base (exact stop IDs) | 6,170 | 2.01 | 1.25 | 19.8 % | 1.27 | 23.5, 32 |
| place (common lines) | 6,170 | 3.56 | 2.73 | 47.6 % | 1.72 | 39.1, 65 |

- The retained corridors are identical in 100/100 scenarios. Place equivalence only widens the
  set of lines that Phase 2 may board on each edge.
- Realtime stops per plan rise by about 66 %, but `tdx.batch.bus_stops = 200` keeps them to one
  TDX request per scope. The live run checks this.
- Latency was **not measurable**: the load average was ~490 (other agents compiling), and base
  p50 was 954 ms against 85 ms on a quiet machine. Rerun on a quiet machine before any PR.

## Research artifact (2026-11-24 01:35)

The Valhalla rebuild is blocked (OrbStack down), so the parent routes and headways are
attached offline to the 2026-11-20 artifact:

```
attach_headways data/build/20261120T163303Z/.via-static.json data/fetch/bus-schedule-20261121 \
  data/build/research-20261124/.via-static.json data/fetch/20261123T170000Z
```

- Bus/Route fetched 2026-11-24 01:02–01:28 with 61 s pacing (`examples/fetch_bus_routes.rs`):
  22 cities, 3,108 routes, **2,958 bindings with a parent route**. The production artifact has 0.
- Headways (Bus/Schedule 2026-11-21): 2,675 patterns from GTFS, 2,125 from TDX, 1,966 without,
  223 unmapped TDX sub-routes; **2,128 Bus bindings carry estimate headways** (no GTFS trips).
- Corridor search on the new artifact is identical to the old one in 100/100 scenarios
  (the network is unchanged; only bindings differ).

## Live run plan (daytime, needs Valhalla on :8002)

```
cd server && VIA_CONFIG=<via.toml with tdx.connect_timeout_ms=5000> VIA_VALHALLA_URL=http://127.0.0.1:8002/ \
  live_compare data/build/research-20261124/.via-static.json ../research/live-scenarios.json out.jsonl \
  base,place,est,place+est
```

Metrics: arrival > 30 s earlier or later vs base, walking-only count, transfers, walk
time, TDX requests per variant, planning latency. ETA accuracy against TDX live: for
each realtime bus leg, compare the departure Via used with a later N1 estimate for the same stop
(follow-up probe).

## Live A/B result (2026-11-24 19:42-21:20 Asia/Taipei, 20 scenarios, 1 TDX plan per 3-7 min)

Command: `VIA_CONFIG=<via.toml: connect 5 s, request 30 s, fresh 600 s, degraded 900 s>
VIA_VALHALLA_URL=http://127.0.0.1:8002/ live_compare data/build/research-20261124/.via-static.json
research/live-scenarios.json out.jsonl base,place,est,place+est` (raw rows:
`research/results/exp008-live-20261124.jsonl`, analysis: `research/results/analyze_live.py`).
The four variants share one realtime cache (TTL 300 s) and rotate order per scenario.
One scenario (random_009) failed with `TemporarilyUnavailable` in all variants; 19 valid.

| variant | earlier by >30 s | later by >30 s | same | walk-only (base 5) | mean arrival change (19) |
| --- | --- | --- | --- | --- | --- |
| place | 2 (-600 s, -208 s) | 0 | 17 | 5 | -40 s |
| est | 3 (-1440 s, -1376 s, -2160 s) | 0 | 16 | 4 | -249 s |
| place+est | 4 (-1440, -1376, -2160, -208) | 0 | 15 | 4 | -259 s |

- No scenario got worse in any variant.
- `est` removes a 3 h walk (xinzhuang_to_muzha: 11,204 s walk-only -> 6,908 s walk + Bus 933 + walk,
  still long) and replaces long walks with a bus leg (ntu_to_nangang, random_006).
- `place` gives two small wins (ntu_to_nangang -10 min, songshan_airport_to_wanhua -3.5 min).
- TDX requests when a variant ran first on a scenario (each plan needs one request per scope):
  base 3,3,5,3,3; place 3,3,3,3,3; est 3,3,4,3,2; place+est 3,3,3,(cached),(cached). The
  66 % more realtime stops per plan did not raise the request count, as predicted.
- Latency is not comparable: the shared cache gives the first variant all TDX time and the
  machine load average was 40-250. Median per plan 6-12 s, almost all TDX/Valhalla wait.
- 5 of 20 scenarios still return walk-only (2-3 h walks, e.g. random_005 8,102 s). In 3 of them
  the best static corridor is a bus-only ride with no realtime prediction at 20:30-21:00, which
  `est` cannot help: it starts after the last fresh prediction. Likely end-of-service buses.

Decision: retain **est** (owner approved B18) and **place** as PR candidates. Caveats: 19 scenarios
in one evening, one time of day; est's accuracy against later TDX observations is not measured
(B23 step 1).
