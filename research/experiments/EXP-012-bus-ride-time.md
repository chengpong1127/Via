# EXP-012: are Via's static Bus ride times realistic? (TDX A2 vehicle positions)

Status: measured at 10:35 (one snapshot); time-of-day runs scheduled 12:30, 15:00, 17:30, 18:30, 20:30 (appended below).
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
