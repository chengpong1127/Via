# EXP-014: use TDX N1 estimates for buses that have not departed (StopStatus 1)

Date: 2026-11-26 evening. Zero TDX requests (replay on the EXP-013 scale-290 artifact, 58 plans, `est` vs `s1`).

## Observation
`normalize_n1` maps `StopStatus` 1 to `NotDeparted` with no time and drops `EstimateTime`. In the recordings 27 percent of
Taipei and New Taipei N1 records are StopStatus 1 with an `EstimateTime` (Taipei 272 vs 486 status-0, New Taipei 204 vs 387);
another 25 percent are status 1 with no estimate. `NextBusTime` is present only for Taoyuan (257 records, always on the minute,
median 42 min ahead); Via ignores it too, but Taoyuan is not where the gaps are. Meaning of StopStatus 1 and of the estimate for it:
not verified against the TDX field table (searches and the community guide did not reach it); the data suggest operator
departure-based estimates for trips not yet started.

## Accuracy of status-1 estimates (daytime ETA log, 2026-11-25 10:47-12:25, `results/eta-log-20261125-day.jsonl.gz`)
Error = predicted minus the next observed arrival at that stop (arrival = status-0 estimate under 90 s).
| horizon of the estimate | n | median error | MAE |
| --- | --- | --- | --- |
| 0-10 min | 475 | -77 s | 328 s |
| 10-20 min | 768 | -69 s | 237 s |
| 20-40 min | 1,435 | -72 s | 197 s |
| over 40 min | 882 | -78 s | 197 s |
Compare status-0 N1 (EXP-010): MAE 70 s at 5-10 min, 120 s at 10-20 min. A bus was seen within 15 min of the predicted time for
70 percent of status-1 estimates of 10 min or more, against 86 percent for status-0 estimates. So status-1 estimates are usable
but weaker (late departures, some trips that never show).

## Replay A/B (variant `s1`: status 1 with a positive estimate becomes `Predicted`)
Prototype: `NOT_DEPARTED_ESTIMATES` toggle in `bus_realtime.rs` on `research/exp-013-post-scale-check`. 58 plans:
2 earlier (banqiao_to_linkou -8 min: Bus 952 then 2000 instead of the 84 min plan; shenkeng_to_jingmei -3 min), 0 later.
`results/exp014-s1-replay-{a,b,c}.jsonl`.

## Why the large gaps remain
sanxia_to_banqiao: the recorded window has bus 910 at the origin stop with status 1 and no estimate. xizhi_to_zoo and luzhou_to_bali
also need buses with no prediction. 25 percent of Taipei/New Taipei records have no timing at all; AGENTS.md allows no estimate
without a fresh prediction anchor, so these are data gaps, not search errors.

## Decision
Marginal: 2 of 58 plans improve (-8, -3 min), none worse, but the estimates are less reliable than status 0. If shipped it
should carry the `estimated` label (Fallback reliability), which makes it an owner decision on AGENTS.md wording. Not PR'd. Parked
in the backlog as B32.
