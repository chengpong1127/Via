# Time-independent street reconstruction

Reconstruction previously sent walking and cycling Route requests with the current journey departure time. It now sends untimed requests, matching the policy already used by the Phase 2 walking and cycling Matrix queries. Valhalla still supplies the full route, including geometry, maneuvers, duration, distance and attribution. Transit schedules and journey assembly remain time-dependent: each candidate applies the route duration to its own current time and checks boarding feasibility and journey limits independently.

This is an intentional routing policy, not a claim that Valhalla always returns the same route at different times. Conditional road access and other time-sensitive restrictions may make an untimed route differ from a timed route. The change does not affect other provider operations or Matrix queries.

A reconstruction request owns one shared cache. Its key uses the exact bits of validated origin and destination coordinates, their direction, travel mode, and all routing constraints. Timed queries bypass the cache. Successful paths and unreachable results are retained; transient and internal failures are removed after completion so later calls can retry. Concurrent callers await a Tokio `OnceCell`; no lock is held during provider work. Dropping a caller cancels its future, and if it was initializing the cell another waiter can initialize it. No background task is spawned. The provider retains its existing timeout and semaphore. The cache admits at most 4,096 entries; new distinct keys bypass it when full, while existing in-flight entries remain available. All state is dropped at the end of the planning request.

## Measurements

The earlier timed, uncached representative run reported 8,161 physical sends for 1,139 Transit Only candidates (29,035 ms at concurrency 4) and 1,145 sends for 242 Transit + BikeShare candidates (3,123 ms). These historical measurements are not a controlled comparison with this change. The ignored `capture_representative_pools` test supports `VIA_ROUTE_CACHE_COMPARE=1` to replay each freshly captured pool with untimed routing both without and with the cache at concurrency 4, using the same candidate pool. It reports ordered candidate and ranked-output digests, provider sends, errors, timeouts, peak HTTP activity and cache statistics. Cache hits comprise ready plus in-flight hits; misses count new keys and capacity bypasses. Provider sends are separately instrumented at the HTTP boundary.

A single release-build replay on 2026-10-28 used Valhalla 3.9.0, concurrency 8, the 1,200 ms provider timeout and reconstruction concurrency 4, with the same in-memory pool for each pair of runs. Timings are single observations and are not a latency guarantee.

| Workload | Candidates | Logical routes | Unique keys | Ready hits | In-flight hits | Physical sends without / with cache | Wall ms without / with cache | Peak HTTP without / with cache |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Transit Only | 1,139 | 8,161 | 87 | 7,927 | 147 | 8,161 / 87 | 18,359 / 490 | 4 / 2 |
| Transit + BikeShare | 242 | 1,145 | 151 | 923 | 71 | 1,145 / 151 | 3,747 / 1,115 | 4 / 4 |

The cache-served fractions, including in-flight reuse, were 98.93% and 86.81%, respectively; physical sends fell by the same percentages in these runs. Neither workload had timeouts, candidate failures, or rejected candidates. Within each workload the ordered candidate digest and complete ranked outcome digest matched between cache modes, including result-local IDs. This does not establish equivalence between the earlier timed policy and the new untimed policy. A controlled comparison of durations, geometry, transit feasibility and Top-3 under those different policies remains outstanding. The fixed candidate pools are generated in memory and are not persisted between runs.

Early Street Hydration during Phase 2 could reduce late feasibility surprises, but would require a separate design and measurement; this change does not implement it.
