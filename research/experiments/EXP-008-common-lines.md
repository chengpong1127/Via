# EXP-008: Common-lines transit edges vs headway-estimated departures

Branch: `research/exp-008-common-lines` (off `origin/main` e1eefeb)
Status: in progress (offline done below; live A/B pending, daytime)
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
