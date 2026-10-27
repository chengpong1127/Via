# Release measurements

Measured on the same workspace host using release binaries. Each case has two warmups and 20 measured iterations; p50, p95 and worst are observed wall times. The provider is deterministic and sleeps asynchronously for each Matrix call; its semaphore permits eight physical operations. Three independent benchmark processes ran concurrently, one per workload, so timings include host scheduling noise. No live TDX or Valhalla traffic was used.

The reference binary was built from the original Phase A commit with only the benchmark fixture added. The current binary uses the request-local resolver and the indicated expansion concurrency. The fixture evaluates a transit corridor with a resolving walking egress; the mixed and BikeShare variants use the real BikeShare planner and real asynchronous walking and bicycle Matrix mock calls. The BikeShare variant has an empty transit snapshot. These are deterministic stress fixtures, not production latency estimates.

## Phase 2 latency

The table shows p50 / p95 in milliseconds for successful queries with a 50 ms provider delay. Physical calls are mock provider operations per evaluation.

| Workload | Phase A | New 1 | New 2 | New 4 | New 8 | Calls: Phase A → new | Max active provider operations |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| walking | 264.03 / 267.00 | 264.36 / 271.43 | 158.12 / 191.19 | 104.69 / 107.22 | 105.85 / 110.99 | 5 → 1:5, 2:5, 4:5, 8:5 | 4 |
| mixed | 475.20 / 485.52 | 423.69 / 433.62 | 266.84 / 270.04 | 211.74 / 319.15 | 210.23 / 216.64 | 9 → 1:8, 2:8, 4:8, 8:8 | 5 |
| bike | 315.97 / 319.26 | 267.43 / 276.95 | 213.10 / 219.97 | 211.28 / 215.16 | 211.19 / 218.12 | 6 → 1:5, 2:5, 4:5, 8:5 | 2 |

P50 / p95 changes relative to Phase A (positive is faster):

| Workload | New 1 | New 2 | New 4 | New 8 |
| --- | ---: | ---: | ---: | ---: |
| walking | -0.1% / -1.7% | 40.1% / 28.4% | 60.3% / 59.8% | 59.9% / 58.4% |
| mixed | 10.8% / 10.7% | 43.8% / 44.4% | 55.4% / 34.3% | 55.8% / 55.4% |
| bike | 15.4% / 13.3% | 32.6% / 31.1% | 33.1% / 32.6% | 33.2% / 31.7% |

Failure and unreachable cases at 50 ms:

| Workload / result | Phase A p50 / p95 ms | New 4 p50 / p95 ms | Calls: Phase A → new 4 | Candidates |
| --- | ---: | ---: | ---: | ---: |
| walking / unreachable | 210.65 / 212.14 | 105.43 / 108.14 | 4 → 4 | 0 |
| walking / timeout | 212.55 / 216.67 | 105.17 / 106.89 | 4 → 4 | 0 |
| mixed / unreachable | 265.54 / 270.37 | 157.20 / 159.60 | 5 → 5 | 0 |
| mixed / timeout | 263.97 / 268.96 | 158.25 / 165.06 | 5 → 5 | 0 |
| bike / unreachable | 106.01 / 108.66 | 106.01 / 109.34 | 2 → 2 | 0 |
| bike / timeout | 106.77 / 108.46 | 105.91 / 109.31 | 2 → 2 | 0 |

Zero-delay successful cases (p50 / p95 in microseconds):

| Workload | Phase A | New 1 | New 2 | New 4 | New 8 |
| --- | ---: | ---: | ---: | ---: | ---: |
| walking | 9 / 10 | 41 / 52 | 51 / 92 | 36 / 54 | 43 / 66 |
| mixed | 21 / 25 | 30 / 38 | 93 / 127 | 81 / 181 | 79 / 182 |
| bike | 12 / 13 | 17 / 30 | 16 / 30 | 49 / 76 | 79 / 176 |

All 0, 10, 50 and 100 ms cases, including failures, produced identical ordered journey fingerprints, successor counts and candidate counts across Phase A and all new concurrency settings. The integration test separately compares reconstructed ordered Top-3 results at 1, 2, 4 and 8. The raw `io-*.txt` files include every delay, p50, p95, worst, logical requests, physical calls, completed/in-flight hits, provider waiting and fingerprints.

## Exact-query reuse and provider contention

The following separate Matrix microbenchmark runs ten logical queries per request at 50 ms. Identical queries use one exact key; distinct queries vary their target coordinates. Multiple requests have independent caches but share one mock provider semaphore. Raw bypasses the new resolver; it is a Matrix-layer comparison and does not model the old movement caches.

| Workload | Resolver / state limit | p50 / p95 ms | Logical / physical calls | Completed / in-flight hits | Peak active | Wait µs |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| identical, 1 request | raw / 1 | 909.47 / 7169.28 | 10 / 10 | 0 / 0 | 1 | 9 |
| identical, 1 request | shared / 1 | 52.66 / 87.16 | 10 / 1 | 9 / 0 | 1 | 0 |
| identical, 1 request | shared / 2 | 52.70 / 56.01 | 10 / 1 | 8 / 1 | 1 | 0 |
| identical, 1 request | shared / 4 | 77.42 / 245.06 | 10 / 1 | 6 / 3 | 1 | 0 |
| identical, 1 request | shared / 8 | 58.45 / 107.16 | 10 / 1 | 2 / 7 | 1 | 0 |
| distinct, 1 request | raw / 1 | 529.65 / 538.28 | 10 / 10 | 0 / 0 | 1 | 7 |
| distinct, 1 request | shared / 1 | 525.84 / 533.98 | 10 / 10 | 0 / 0 | 1 | 0 |
| distinct, 1 request | shared / 2 | 264.00 / 270.89 | 10 / 10 | 0 / 0 | 2 | 0 |
| distinct, 1 request | shared / 4 | 169.41 / 339.09 | 10 / 10 | 0 / 0 | 4 | 0 |
| distinct, 1 request | shared / 8 | 142.90 / 247.51 | 10 / 10 | 0 / 0 | 8 | 0 |
| distinct, 4 requests | raw / 1 | 529.71 / 562.64 | 40 / 40 | 0 / 0 | 4 | 6 |
| distinct, 4 requests | shared / 1 | 532.35 / 536.54 | 40 / 40 | 0 / 0 | 4 | 0 |
| distinct, 4 requests | shared / 2 | 262.79 / 266.87 | 40 / 40 | 0 / 0 | 8 | 0 |
| distinct, 4 requests | shared / 4 | 371.04 / 781.45 | 40 / 40 | 0 / 0 | 8 | 13843825 |
| distinct, 4 requests | shared / 8 | 290.25 / 661.55 | 40 / 40 | 0 / 0 | 8 | 5112041 |
| identical, 4 requests | raw / 1 | 785.13 / 4206.22 | 40 / 40 | 0 / 0 | 4 | 32 |
| identical, 4 requests | shared / 1 | 53.33 / 57.25 | 40 / 4 | 36 / 0 | 4 | 0 |
| identical, 4 requests | shared / 2 | 52.79 / 56.07 | 40 / 4 | 32 / 4 | 4 | 0 |
| identical, 4 requests | shared / 4 | 70.30 / 151.35 | 40 / 4 | 24 / 12 | 4 | 0 |
| identical, 4 requests | shared / 8 | 54.75 / 104.99 | 40 / 4 | 8 / 28 | 4 | 0 |

The mock peak is counted after semaphore acquisition; it is independent of the resolver’s delegated-operation peak. `wait_us` is the accumulated semaphore wait in the final iteration, not a percentile. Waiting and overlap can vary by scheduling. Identical queries eliminate nine of ten provider operations per request; separate requests do not share results.

The raw identical-query series had a 7.17 s p95 and a 31.49 s worst case despite a 50 ms simulated delay. These host scheduling outliers make its latency comparison unreliable; the operation counts remain deterministic.

## Zero-delay CPU

The CPU measurement uses ten separate runs of the retained zero-delay Phase A stress benchmark, including process startup and both its 32- and 2,400-corridor fixtures. It exercises mostly completed Walking cache hits, so it is a coarse CPU overhead indicator rather than the asynchronous-provider workload above. CPU time is measured from child-process user and system time.

```text
reference runs=10 median_wall_s=0.193407 median_user_s=0.173954 median_system_s=0.016871 total_cpu_s=2.100135
current runs=10 median_wall_s=0.411957 median_user_s=0.372742 median_system_s=0.035742 total_cpu_s=4.710685
```

Total measured CPU time for this stress test increased by 124% (2.24×). This includes process startup and is specific to this mostly cached fixture.

## Reproduction

Run `python3 docs/performance/p2-parallel-expansion/prepare_reference.py` to create a temporary reference checkout and add the fixture; it prints the reference manifest path. Run `cargo test --release --manifest-path <reference-manifest> --lib --no-run` and copy the executable path printed by Cargo to `/tmp/via-phase-b-reference.bin`. Run `cargo test --release --manifest-path server/Cargo.toml --lib --no-run` and copy its printed executable to `/tmp/via-phase-b-current.bin`. Then run:

```bash
python3 docs/performance/p2-parallel-expansion/run_comparison.py
python3 docs/performance/p2-parallel-expansion/render_measurements.py
```

The comparison script accepts `VIA_REFERENCE_BINARY` and `VIA_CURRENT_BINARY` to select other paths. It limits concurrent benchmark processes to three and runs the CPU measurements separately. The original pre-change release logs are also retained as `phase-a-*.txt` and `io-baseline.txt`.
