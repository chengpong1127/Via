# Phase B.1: time-independent surface matrices

The Phase B commit `258c8c57622931edf156f301a1684c1e3fdb4719` and the updated implementation were measured using the same release fixture, with two warmups and 20 measured iterations, a mock provider with eight permits, delays of 0, 10, 50 and 100 ms, and concurrency 1, 2, 4 and 8. The walking and mixed fixtures create multiple transit states at different times that reuse the same walking edge; the bike fixture uses real BikeShare realization. Separate processes ran four at a time while the host was also compiling. These are noisy wall times, not production estimates; the mixed baseline p95 at concurrency 4 is an obvious scheduling outlier. No external provider was called.

Successful cases with 50 ms simulated provider delay, p50 / p95 in milliseconds:

| Fixture | Concurrency | Before | After | Physical calls | Logical resolver calls | Peak provider operations |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| walking | 1 | 264.76 / 276.86 | 52.59 / 55.36 | 5 → 1 | 5 → 2 | 1 → 1 |
| walking | 2 | 156.70 / 161.59 | 52.80 / 55.53 | 5 → 1 | 5 → 2 | 2 → 1 |
| walking | 4 | 105.54 / 107.77 | 52.96 / 55.74 | 5 → 1 | 5 → 2 | 4 → 1 |
| walking | 8 | 103.90 / 106.23 | 51.93 / 54.06 | 5 → 1 | 5 → 2 | 4 → 1 |
| mixed | 1 | 424.67 / 460.43 | 157.97 / 163.25 | 8 → 3 | 9 → 5 | 1 → 1 |
| mixed | 2 | 261.77 / 268.97 | 157.59 / 159.82 | 8 → 3 | 9 → 5 | 2 → 1 |
| mixed | 4 | 272.96 / 1038.39 | 158.99 / 162.66 | 8 → 3 | 9 → 5 | 4 → 1 |
| mixed | 8 | 219.96 / 259.72 | 157.01 / 160.15 | 8 → 3 | 9 → 5 | 5 → 1 |
| bike | 1 | 264.15 / 274.48 | 158.68 / 216.13 | 5 → 3 | 6 → 5 | 1 → 1 |
| bike | 2 | 209.76 / 229.22 | 161.14 / 216.63 | 5 → 3 | 6 → 5 | 2 → 1 |
| bike | 4 | 209.88 / 252.44 | 160.26 / 185.70 | 5 → 3 | 6 → 5 | 2 → 1 |
| bike | 8 | 209.38 / 214.55 | 164.73 / 175.20 | 5 → 3 | 6 → 5 | 2 → 1 |

Every success, unreachable and timeout case at every delay and concurrency retained identical mock journey fingerprints, successor counts and candidate counts. Full results, including zero-delay latency, Matrix completed and in-flight hits, semaphore wait, bicycle calls and CPU measurements, are in [the raw log](phase-b1-raw.txt). The earlier Phase B resolver already shared identical complete queries; B.1 removes the time distinction and enables additional movement-level reuse. For walking at concurrency 4, physical calls fell from five to one. The after result includes one completed Matrix hit between distinct anchor identities with identical coordinates; the remaining reuse happens at the Walking cache. BikeShare matrices were already time-independent.

The 2,400-corridor zero-delay CPU fixture reuses the same anchor pair even before this change. Five process runs per version produced the following median user / system CPU seconds; the workload and concurrent host activity limit the interpretability of these figures:

```text
before median_wall_s 0.5174858720274642 median_user_s 0.399181 median_system_s 0.04752500000000001
after median_wall_s 1.9030674629611894 median_user_s 0.8490630000000001 median_system_s 0.15919999999999995
```

The benchmark binary used here reports logical and physical Matrix counts and Matrix hits, but does not print movement-level Walking or BikeShare hits. The regression tests separately verify Walking cache hits and concurrent single-flight. The existing 2,400-corridor fixture verifies reuse across corridors; it is not an additional timed multi-corridor latency comparison. No real Valhalla comparison was performed. Omitting `date_time` deliberately changes the model and may alter route choice, reachability, duration or distance on time-restricted infrastructure; equivalent mock outputs do not validate those real-world effects.
