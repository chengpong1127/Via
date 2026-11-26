# EXP-015: fetch Bus realtime scopes concurrently

Date: 2026-11-26 evening. Zero TDX requests (replay with an injected response delay).

## Observation
`BusDepartureSource::realtime_departures_batch` looped over the realtime scopes with `.await` inside the loop, so one plan with
Bus legs in Taipei, New Taipei and Taoyuan paid three TDX round trips in series. The 58 recorded plans (EXP-011 sets A, B, A repeated)
need 1 scope in 4 plans, 2 in 25, 3 in 12, 4 in 16 and 5 in 1. Request start times in the recorder: banqiao_to_linkou +0.88 s,
+1.21 s, +1.36 s (NewTaipei, Taipei, Taoyuan); first-to-last request span in a plan: median 0.76 s, p90 2.46 s.
Live plan latency in EXP-011 (loaded machine, first variant, cold realtime cache, n=58): p50 1,726 ms, p95 5,803 ms; 3 TDX requests per plan at the median.

## Method
Prototype toggle `PARALLEL_SCOPES` in `bus.rs` (`research/exp-013-post-scale-check`), variants `est` (sequential) and `est+par`
(`join_all`), each in its own process (`VIA_CACHE_TTL=1`) over the three replay sets on the scale-290 artifact, with
`REPLAY_DELAY_MS` per TDX response in `tools/tdx_replay.py` as the stand-in round trip. Results in `results/exp015/`.

| round trip | sequential p50 / p95 / mean | concurrent p50 / p95 / mean | n |
| --- | --- | --- | --- |
| 150 ms | 837 / 3132 / 1063 ms | 464 / 629 / 473 | 58 |
| 300 ms | 885 / 1294 / 883 | 623 / 812 / 640 | 116 |
| 500 ms | 1418 / 4609 / 1788 | 940 / 1552 / 1027 | 58 |
Plan results are identical (116 of 116 first-option arrivals at 300 ms); TDX requests per plan 3.1 in both. The first 300 ms run (shared
cache, 90 s TTL, rotated order) was confounded by cache hits and discarded. Sequential p95 includes load spikes (machine load 13-20).

## Decision
Retained: PR #55 `perf/parallel-bus-scopes` (test red then green; fmt, clippy clean; one unrelated flaky timing test in `data/fetch.rs`).
Not measured: real TDX round trips in production. The request count is unchanged, so the 5 req/min budget is not affected, but
simultaneous bursts to TDX at the start of a wave are new behaviour.
Next latency candidates: three request waves in some plans (banqiao_to_linkou), each a full round trip; whether the first wave can
include the second wave's scopes.
