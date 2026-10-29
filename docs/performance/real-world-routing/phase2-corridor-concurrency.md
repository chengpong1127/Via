# Phase 2 corridor concurrency

`routing.max_concurrent_phase2_corridors` sets the maximum number of Phase 2
corridors in flight per planning request. It accepts 1–8 and defaults to 1;
older configuration files without the setting also use 1. State expansion
remains bounded separately by `max_concurrent_phase2_states` (default 4),
so increasing corridor concurrency can increase demand on the shared Valhalla
semaphore. The existing request-local walking, BikeShare and Matrix caches are
shared across corridors. Successful results are merged in original corridor
order before the global Pareto pass. The first fatal error observed in completion
order ends the stream and cancels other in-flight corridors; error precedence
can therefore vary when failures race. The request deadline wraps the whole
stream and also cancels unfinished work. On error or timeout, statistics and
failure flags include outcomes consumed before termination, including a fatal
corridor if its outcome was received. Partially evaluated corridors canceled
before their outcome is received do not contribute their local counters or
failure flags; request-local cache and provider counters still reflect their
work. The attempted count records corridors admitted to the stream, and the
completed count records successful corridor outcomes consumed before termination.

The ignored `phase2_corridor_representative_benchmark` uses the existing
2026-10-01 static artifact and fixed Taipei Main Station–Xinyi workload,
static schedule timing, fixture BikeShare availability and Valhalla at
`http://valhalla:8002/`. It makes no TDX calls. It keeps state concurrency at 4
and provider concurrency at 8. Each session has fresh request-local caches;
preparation is outside the Phase 2 wall time. Four rotated rounds per setting
include one warmup and three measured samples. The benchmark also reconstructs
and ranks the final round outside the measured interval. It records Matrix
initializations and actual HTTP starts separately, and reports the sum of
semaphore waits rather than request wall time. This is a historical, fixed
workload, not live availability or current schedules.

```bash
cd server
CARGO_BUILD_JOBS=1 VIA_CHROME_TRACE=/tmp/via-phase2-corridors.trace.json \
  cargo test --release --locked --lib phase2_corridor_representative_benchmark \
  -- --ignored --nocapture
```

Open the resulting trace in Perfetto and inspect overlapping `Phase 2 Corridor`
spans. Profiling adds overhead, and semaphore wait sums across concurrent calls.

## Phase D.2 benchmark repeat

These measurements were recorded at commit `a2355d9`, before the buffered-stream
refactor, and have not been repeated for the new scheduler.

This repeat ran after the deadline fix and after debug builds completed. Each
median uses three measured samples after one warmup; this is too few for a
meaningful p95. Profiling remained enabled and host and provider load were not
otherwise controlled. Hits and summed wait show the range across samples.

| Workload | Corridors | P2 p50 ms | States | Successors | Candidates | Matrix HTTP | Ready / in-flight hits | Wait ms | Timeouts | Peak HTTP |
| --- | ---: | ---: | --- | --- | --- | --- | --- | --- | --- | ---: |
| Transit | 1 | 1,565.3 | 4,175 | 5,896 | 1,139 | 51 | 35 / 0 | 0.26–0.54 | 0 | 1 |
| Transit | 2 | 1,406.2 | 4,175 | 5,896 | 1,139 | 51 | 29 / 6 | 0.14–0.33 | 0 | 2 |
| Transit | 4 | 1,737.5 | 4,175 | 5,896 | 1,139 | 51 | 19 / 16 | 0.26–0.48 | 0 | 4 |
| Transit | 8 | 1,393.9 | 4,175 | 5,896 | 1,139 | 51 | 23–25 / 10–12 | 0.17–0.48 | 0 | 6 |
| Transit + BikeShare | 1 | 5,259.5 | 10,242 | 101,132 | 242 | 279 | 571 / 4 | 0.95–1.76 | 0 | 3 |
| Transit + BikeShare | 2 | 4,345.0 | 10,242 | 101,132 | 242 | 279 | 515 / 60 | 1.03–2.12 | 0 | 4 |
| Transit + BikeShare | 4 | 3,380.1 | 10,242 | 101,132 | 242 | 279 | 459–460 / 115–116 | 1.53–2.54 | 0 | 7 |
| Transit + BikeShare | 8 | 3,343.8 | 10,242 | 101,061–101,132 | 242–269 | 273–279 | 443–454 / 121–125 | 87.91–104.66 | 7 | 8 |

All Transit Only candidate digests matched. The mixed digests matched for
concurrency 1, 2 and 4; at 8, one sample had seven provider timeouts and a
different candidate digest. The other two 8 samples matched. Final-round
Top-3 key digests matched at every setting for both workloads (one Transit
Only option and three mixed options), with no reconstruction errors. The
[final raw results](benchmarks/phase2-corridor-final.txt) include all samples
and digests. Matrix initialization counts matched HTTP starts, and all
recorded provider failures were timeouts.

The final trace is `/tmp/via-phase2-final.trace.json`; parsing its 117,973
events found 2,880 paired corridor spans and a maximum overlap of eight.
Perfetto can display the JSON, though the UI was not opened in this environment.
The median at four was approximately 36% faster than the median at one for
the mixed workload, while eight added little median benefit over four,
reached the provider concurrency cap and showed substantially greater wait.
Keep the default at **1**: this small sample and the loaded run below do not
establish reliable behavior under realistic request deadlines or concurrent
user requests. Test 2 or 4 under controlled representative production load
before changing it.

## Earlier load-affected sweep

The table reports the median of three measured repetitions; the sample is too
small for a meaningful p95. Cache columns are ranges of ready/in-flight hits
across those repetitions. Wait is the range of summed semaphore wait per
sample. Failures are Matrix operation timeouts across the three repetitions.
The host was under substantial memory pressure and the debug test build ran
during part of this sweep. Chrome tracing was enabled. The benchmark executable
was built before a subsequent deadline-accounting correction; deadlines were
disabled in these samples, so the correction did not affect the intended
workload, but the final code has not been remeasured. These numbers are not a
controlled production latency comparison.

| Workload | Corridors | P2 p50 ms | States | Successors | Candidates | Matrix HTTP | Ready / in-flight hits | Wait ms | Timeouts | Peak HTTP |
| --- | ---: | ---: | --- | --- | --- | --- | --- | --- | --- | ---: |
| Transit | 1 | 1,447.6 | 4,175 | 5,896 | 1,139 | 51 | 35 / 0 | 0.16–0.50 | 0 | 1 |
| Transit | 2 | 1,635.3 | 4,175 | 5,896 | 1,139 | 51 | 29 / 6 | 0.10–0.75 | 0 | 2 |
| Transit | 4 | 1,711.0 | 4,175 | 5,896 | 1,139 | 51 | 19–23 / 12–16 | 0.19–0.83 | 0 | 4 |
| Transit | 8 | 1,316.3 | 4,175 | 5,896 | 1,139 | 51 | 18–24 / 11–17 | 0.26–0.87 | 0 | 6 |
| Transit + BikeShare | 1 | 19,945.7 | 10,242 | 100,286–101,132 | 238–242 | 279 | 571 / 4 | 6.42–69.71 | 5 | 3 |
| Transit + BikeShare | 2 | 17,209.0 | 10,242 | 100,196–101,132 | 242–251 | 272–279 | 499–516 / 56–59 | 8.17–15.76 | 10 | 5 |
| Transit + BikeShare | 4 | 22,082.3 | 10,125–10,242 | 88,232–101,105 | 242–690 | 260–279 | 382–458 / 102–117 | 11.24–32.35 | 62 | 7 |
| Transit + BikeShare | 8 | 20,565.0 | 9,390–10,242 | 85,695–101,132 | 242–553 | 259–279 | 369–450 / 104–125 | 234.21–853.04 | 40 | 8 |

Physical Matrix initialization counts matched HTTP starts in these samples;
all recorded failures were timeouts. The Transit Only candidate digest stayed
identical. Mixed samples without timeouts shared the same candidate digest;
timeouts caused candidate and work-count differences, including at concurrency
1. All four final-round reconstructed Top-3 key digests matched within each
workload: Transit yielded one ranked option and mixed yielded three, with no
reconstruction errors. Top-3 key equality is not a proof of full journey-option
equality; the fixture integration test compares complete options.

The [raw results](benchmarks/phase2-corridor-run.txt) include warmup and per-run
counts. The generated trace is `/tmp/via-phase2-corridors.trace.json`; parsing
its 119,429 events found 2,880 paired corridor spans and a maximum overlap of
eight. This trace is from the benchmark executable before the deadline fix.

Retain the default of **1** until a clean, final-code comparison under stable
provider load and representative request deadlines establishes a safe higher
setting. The mixed 4 and 8 settings showed more total timeouts and contention,
while all settings experienced some timeouts in this run. Three samples per
setting cannot establish production tail latency or attribution of every
failure to concurrency.
