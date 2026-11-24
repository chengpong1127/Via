# EXP-010 (B23 step 1): TDX bus N1 ETA error against later TDX observations

Date: 2026-11-24 21:26-21:58 Asia/Taipei. Branch: `research/exp-008-common-lines` (`examples/eta_logger.rs`).
Data: `research/results/eta-log-20261124-evening.jsonl`, analysis `research/results/eta_analyze.py`.

Method: poll Bus/EstimatedTimeOfArrival for 150 stops per city (Taipei, New Taipei) every 60 s (2 TDX
requests/min, no other TDX process). For each (route, stop, direction) series, one bus ends where the
estimate falls to 120 s or less and the next reading jumps up by more than 3 min. Actual arrival is
proxied by update_time + estimate at the last reading of that bus; error = predicted - actual.

Result (81 completed buses, status 0 rows only):

| horizon before arrival | n | bias | MAE | p90 abs error |
| --- | --- | --- | --- | --- |
| 0-5 min | 363 | +12 s | 33 s | 84 s |
| 5-10 min | 286 | +27 s | 76 s | 152 s |
| 10-20 min | 273 | -8 s | 93 s | 193 s |
| 20+ min | 52 | -135 s | 181 s | 334 s |

Reading: within 10 min the N1 estimate is good (MAE under 80 s) and unbiased; beyond 20 min buses
arrive about 2 min later than predicted. No correction is justified yet.

Caveats: one evening hour (most routes ended: status 3 was 4,412 of 9,900 rows), 81 buses, proxy for
the actual arrival, n at 20+ min is 52. Rerun 08:00-10:00 and at the evening peak before drawing a
conclusion. Via's boarding decision uses horizons of 0-10 min, where error is small.
