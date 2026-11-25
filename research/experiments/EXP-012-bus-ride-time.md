# EXP-012: are Via's static Bus ride times realistic? (TDX A2 vehicle positions)

Status: measured 10:35 (Taipei + New Taipei, 4 rounds), 11:00 (8 cities) and 12:30 (8 cities). NOT run: 15:00, 17:30, 18:30, 20:30 (stopped at owner request), so no peak-hour factor yet.
Date: 2026-11-25. Tools: `tools/a2_snap.py` (collect), `tools/exp012_trip.py`, `tools/exp012_speed.py` (analysis).

## Why

EXP-011 showed Via 10-22 minutes faster than Google on bus journeys (Sanchong to Ximen: Via 9 min with Bus 640 for 328 s, Google 25 min),
while it was far slower than Google where a Metro line is missing. Bus ride time in the artifact is `static_profile.segments`: of 209,107
Bus segments in the 2026-11-25 artifact, 161,245 are `derived` (GTFS first-departure to last-arrival run time spread over the stops by
distance), 47,834 `schedule`, 28 `estimated` (60 km/h straight line). All Taipei (20,326) and New Taipei (28,444) segments are `derived`.
The implied straight-line speed of derived segments is median 40 km/h (p10 36, p90 56). Real city buses are far slower.

## Method

Ground truth: TDX `Bus/RealTimeNearStop/City/{Taipei,NewTaipei}` (A2, one GET per city, 4 rounds 45 s apart, 8 requests in 6 minutes,
10:35-10:41). Each record gives a bus (PlateNumb), its route/direction, current StopUID and StopSequence, GPSTime and TripStartTime
(departure from the first stop). For each bus on duty at stop j >= 6 of its pattern (matched by StopUID and checked against
StopSequence within one) compare
- actual: GPSTime - TripStartTime, with
- model: sum of the artifact's segment seconds from the first stop to stop j.
No Via code is involved, no timetable is invented: it only asks whether the static segment seconds Via uses are the right size.

## Result (4,852 bus observations, 3,143 buses on duty, Taipei + New Taipei, 10:35)

- Actual / model: pooled 2.87, median 2.97, p10 2.14, p90 3.82. Taipei 3.07, New Taipei 2.78.
- Independent of trip length: median 2.86 (stops 6-9), 3.04 (10-19), 3.01 (20-39), 2.91 (40+). By model length bucket 2.94 / 3.05 /
  3.02 / 2.90 (0-5 min, 5-10, 10-20, 20-40 min).
- OLS: actual = 478 s + 2.30 x model (the 478 s is terminal dwell/layover after TripStartTime).
- The short-baseline version (same bus seen twice within 4.5 min) gives 2.9 on 1,205 buses at 2+ stops, falling to 1.8 for 6+ stops
  (n=78): short windows are biased by +-1 stop of position error, so the trip-start version above is the one to trust.
- Median observed pace 0.68 stops/min (one stop per 88 s) vs model 2.26 stops/min (one per 27 s).

## Effect on plans (replay with a scaled artifact, zero TDX)

Multiplying every `derived`/`estimated` Bus segment by 2.5 and replaying the EXP-011 recording (same 20 scenarios, 4 variants):
- Sanchong to Ximen: Via 9 -> 17 min (Google 25). Songshan station to Tamkang: 113 -> 86 (Google 75).
- ntu_to_nangang: place win vanishes (59.3 = base), est win -14 min remains.
- est wins over 30 s: 6 -> 3 (ntu, linkou, xizhi); yongning, songshan_airport, songshan_station disappear.
- place+est over est: xizhi -11 min stays, linkou gone.
- banqiao_to_linkou gets worse (50 -> 84 min vs Google 33): a direct Google bus missing from the artifact (948), now exposed.
Mean gap to Google over 20 scenarios barely moves (+22 vs +25 min for base), because the Wenhu-Line gaps dominate; excluding the four
Wenhu scenarios the base MAE is the same 16.6 minutes: total time is a blunt metric here, the per-leg measurement above is the evidence.

## Effect against Google on 38 scenarios (replay, both EXP-011 sets, Google queried minutes after each plan)

Gap = Via door-to-door minutes minus Google (top-listed option, arrive minus query time); negative = Via faster.

| artifact | variant | median gap | mean gap | MAE | Via >10 min faster | Via >10 min slower |
| --- | --- | --- | --- | --- | --- | --- |
| as built (x1) | base | -0.3 | 11.6 | 20.2 | 6 | 12 |
| as built (x1) | place+est | -5.9 | 2.7 | 17.4 | 10 | 7 |
| Bus segments x2.5 | base | 1.4 | 13.1 | 19.0 | 3 | 15 |
| Bus segments x2.5 | place+est | 0.8 | 9.0 | 15.4 | 3 | 14 |

Excluding the 5 scenarios that need the Wenhu Line (BR has no trips in the feed): place+est median gap -7.2 -> 0.0 min, MAE 10.1 -> 10.1,
"Via over 10 min faster than Google" 9 -> 3. The calibration removes the optimism (Via claiming trips Google calls impossible);
what remains is coverage (BR line, express buses). Both sets and recordings: `results/`.

## Recommendation (for the manager)

1. Bus ride-time calibration should be an engineer task: the static profile should scale `derived` Bus segments by a measured factor
   (config value plus a documented calibration command), not hard-code 2.9. A single factor needs the time-of-day runs below.
2. Do not read EXP-008/EXP-011 est and place wins as final until re-measured on the calibrated artifact (replay does this at no TDX cost).
3. Within AGENTS.md: no timetable is invented; derived segment costs remain "not timetable data", only their scale changes. Owner may
   want to confirm.

## Caveats

- One snapshot, one time of day (Wednesday 10:35, off-peak). Peak hours are probably slower, so the factor may be larger then.
- TripStartTime may include departure delay at the terminal (the OLS intercept captures part of it).
- A2 only covers Taipei and New Taipei.

## Time-of-day and city data so far (derived/estimated segments only, trip-start method)

| snapshot | n | median ratio | notes |
| --- | --- | --- | --- |
| 10:35 (4 rounds) | 4,852 | 2.97 | TPE 3.07, NWT 2.78 |
| 11:00 (8 cities) | 1,700 | 2.79 (derived only 2.94 pooled with 10:35) | TPE 3.06, NWT 2.72, TAO 2.53, TNN 2.13 |
| 12:30 (8 cities) | 1,330 | 2.78 | TPE 3.04, NWT 2.79, TAO 2.49, TNN 2.15 |

City factors differ (Taipei 3.0, New Taipei 2.8, Taoyuan 2.5, Tainan 2.1); midday is about 6 percent lower than 10:35. Held-out-route check
(routes split in two halves): median absolute percentage error of the leg time 66 percent as built, 15 percent with one global factor
(2.91), 14 percent with per-city factors, so a single factor already takes almost all of the benefit. Taichung, Kaohsiung, Keelung,
Hsinchu returned too few derived-segment observations to judge (most of their segments are `schedule`). AM peak (08:00-09:00) and PM peak
(17:00-19:00) are unmeasured.

## Spec for the B28 engineer task

1. Config: `preprocessing.bus_derived_segment_scale_percent` (u32, 100 = unchanged, validated > 0), default in `server/config/via.toml`.
   Initial value 290 (pooled median 2.94 over Taipei, New Taipei, Taoyuan, Tainan; Taipei alone 3.05). Refine per city only if the 15:00-20:30
   runs show a period or city spread above 15 percent.
2. Where applied: at build, in `build_from_fetch_snapshot` right after `gtfs::build_network`, through `TransitNetwork::scale_bus_segment_estimates(percent)`:
   Bus routes only; segments with source `Derived` or `Estimated` get `ceil(seconds * percent / 100)`; `Schedule` segments and all other
   modes are untouched; the static network revision changes. Phase 1 and Phase 2 then agree because both read `static_profile.segments`.
   Reference implementation (tests pass, fmt/clippy clean, full `cargo test` green, AGENTS.md/CONTEXT.md/configuration.md updated):
   branch `research/bus-ride-time-scale-reference` (ef3ba6b), to be rebuilt as the PR off `main`.
3. Measurement (calibration command): `python3 server/docs/calibration/bus_ride_time.py collect out.jsonl [City ...]` (one TDX request per city, 45 s
   apart, needs TDX_CLIENT_ID/SECRET), then `analyze <artifact.json> out.jsonl ...`. It compares time since `TripStartTime` with the artifact's
   summed segment seconds up to the bus's current stop (stop 7 or later) and prints median, p10/p90, per city and per hour, and the suggested percent.
4. Acceptance on the replay harness (zero TDX; recordings in `research/results/`): build the artifact with the new default, replay the 38 EXP-011
   scenarios (sets A and B), variants base and place+est, and compare with the Google minutes. Targets, from the x2.5 prototype: median gap to Google
   (Via minus Google) within +/-3 minutes (from -5.9), plans where Via is more than 10 minutes faster than Google 10 -> at most 4, est wins over base
   11 -> about 5, nothing later than base by more than 30 s. Also assert on the held-out A2 snapshot (any snapshot not used to pick the value) that the
   median actual/model ratio after scaling is within 0.85-1.15.
5. Not in scope: per-period (peak/off-peak) factors need a period-dependent profile (runtime change in `providers/timing/bus.rs` and in Phase 1 costs);
   do it only if peak data show a spread above 15 percent. TDX A2 covers only cities with bus operators reporting positions.
