# Via route tests

The deterministic route corpus spans routing unit tests in `server/src/routing`
and scenario tests in `server/tests/routing`. It uses provider-neutral network,
timing, and street fixtures and does not need TDX, Valhalla, or network access.

| Behavior | Representative test | Assertion |
| --- | --- | --- |
| Metro-only | `simple_mrt_uses_the_fixture_schedule_end_to_end` | One Metro leg, no transfer |
| Bus-only | `simple_bus_uses_the_earliest_realtime_departure` | Fresh Bus timing evidence |
| Bus ↔ MRT | `bus_to_mrt_transfer_keeps_the_bus_then_metro_order` | Transit order and transfer are preserved |
| Same-stop MRT transfer | `mrt_to_mrt_transfer_reconstructs_one_transfer` | Same-stop transfer time applies |
| Multi-transfer | `three_transit_services_reconstruct_with_two_transfers` | Three transit legs remain ordered |
| Equivalent Pattern | `phase2_allows_equivalent_pattern_substitution` | Matching Patterns can supply a retained leg |
| Corridor structure | `phase2_never_combines_different_retained_corridors` | Phase 2 does not recombine corridors |
| YouBike can replace transit | `bike_replacing_a_transit_leg_requeries_the_next_departure` | Bike arrival at an anchor selects the newly reachable downstream departure |
| Cross-anchor YouBike | `one_bike_movement_can_skip_multiple_transit_anchors` | A continuous Bike rental can lazily extend from its original anchor to a later corridor anchor and remains one rental |
| Surface alternative | `bike_can_replace_a_surface_walking_edge` | BikeShare can replace a retained walking edge through the surface movement boundary |
| Hydration safety | `unhydrated_walking_does_not_discard_a_distinct_bike_path`, `transit_remains_when_it_reaches_the_anchor_before_bike` | Distinct unconfirmed street paths remain eligible until reconstruction; final ranking applies dominance |
| Pareto tradeoff | `pareto_keeps_a_later_route_with_less_walking` | Earlier high-walking and later low-walking candidates both remain |
| Bike access and return walks | `faster_bike_surface_transfer_catches_the_next_transit_departure` | Station access and return walking are counted |
| Bike availability | `no_bike_at_pickup_keeps_the_walking_transfer`, `no_dock_at_dropoff_keeps_the_walking_transfer` | Bike use requires an available pickup bike and return dock |
| Bike candidate bounds | `station_choices_are_bounded_to_three_nearest_per_anchor`, `one_bike_movement_can_skip_multiple_transit_anchors` | Only the next structural anchor is checked; there is no eager bounded forward-pair enumeration |
| Direct walking | `direct_walking_is_returned_when_no_transit_access_is_reachable` | When transit has no usable access, the normal evaluation pass includes a direct Origin-to-Destination corridor and returns its feasible walking movement |
| Direct BikeShare | `direct_walking_without_a_transit_query_can_use_bike_share` | The direct walking corridor can realize BikeShare with the request-local availability snapshot |

For the standard checks, run from the repository root:

```bash
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path server/Cargo.toml --all-targets --all-features
git diff --check
```

See [`dev-tooling.md`](dev-tooling.md) for deterministic CLI routes and live
provider configuration.

The direct corridor competes with successful transit in
`viable_transit_corridors_compete_with_direct_walking` and
`viable_transit_corridor_competes_with_direct_bike_share`.
`reconstruction_failure_preserves_direct_walking_without_reloading_snapshots`
keeps a Matrix-realized slower walk when transit geometry fails.
`direct_corridor_cost_and_shared_budget_are_bounded` compares transit-only and
combined P2 work over 100 iterations and checks the exact shared budget boundary.
Run its release measurements with:

```bash
cargo test --manifest-path server/Cargo.toml --release --lib direct_corridor_cost_and_shared_budget_are_bounded -- --nocapture
```

`ranking_considers_three_corridors_before_applying_a_one_result_limit` checks
that two earlier fallback-timed MRT routes cannot exclude a later preferred
walk, and that ranking rejection of both MRT routes still leaves the walk.
`exhausted_transit_corridor_preserves_completed_direct_walking_and_bike_share`
checks surface fairness, the hard budget, and omission of incomplete transit.
`deadline_remains_a_hard_failure_after_a_completed_corridor` uses paused time
to verify that partial-result handling never suppresses a deadline.

Run both release-mode routing and reconstruction benchmarks with:

```bash
cargo test --manifest-path server/Cargo.toml --release --test routing benchmark -- --ignored --nocapture --test-threads=1
```

The surface simplification compares `c5642b3` with the updated implementation
using release-mode, in-memory fixtures. The ranking benchmark emits one diagnostic
request with P2/reconstruction tracing, then measures 1,000 requests without that
subscriber. These are fixture measurements, not real Valhalla measurements.
A 1×1 Matrix is not assumed faster than Route.

| Ranking fixture work per request | Before | After |
| --- | ---: | ---: |
| P2 Walking Matrix requests | 0 | 3 |
| P2 Walking Route requests | 3 | 0 |
| Reconstruction Route requests | 4 | 5 |
| Total Matrix requests, including discovery | 2 | 5 |
| Total Route requests | 7 | 5 |
| BikeShare Matrix requests | 0 | 0 |
| Walking cache hits | 0 | 0 |
| BikeShare cache hits | 0 | 0 |
| P2 expansions | 9 | 9 |
| Successfully reconstructed candidates | 3 | 3 |

A sequential run after compilation, using the original untraced binary and the
updated binary, produced the following latency measurements. Each measured case
uses 1,000 iterations; the updated diagnostic iteration is excluded. These
host-dependent values show a regression in these fixtures, not a provider latency
comparison.

| Fixture | Before median / p95 | After median / p95 |
| --- | ---: | ---: |
| Ranking, eligible transit, end-to-end | 41 / 72 µs | 66 / 81 µs |
| Ranking, rejected transit, end-to-end | 43 / 109 µs | 68 / 114 µs |

| Planner-only fixture | Before median | After median | Before / after P2 expansions | Before / after P2 candidates |
| --- | ---: | ---: | ---: | ---: |
| MRT | 15.750 µs | 23.708 µs | 5 / 7 | 1 / 3 |
| Bus | 14.208 µs | 19.625 µs | 5 / 6 | 1 / 2 |
| Bus → MRT | 33.042 µs | 56.043 µs | 8 / 10 | 1 / 3 |
| MRT → Bus | 46.084 µs | 87.918 µs | 11 / 29 | 1 / 1 |
| Multiple transfers | 44.209 µs | 171.378 µs | 11 / 37 | 1 / 9 |

The planner-only cases do not reconstruct or rank. Their old P2 Walking Route
counts are 2 / 2 / 3 / 4 / 4; updated P2 Walking Matrix counts are
2 / 2 / 3 / 8 / 8. Updated Walking cache hits are 0 / 0 / 0 / 2 / 2,
compared with zero before. BikeShare requests and cache hits are zero throughout.
The larger pools and extra expansions are the cost of keeping distinct paths and
later departures until their actual street timing is known. Candidate counts
remain bounded; the MRT → Bus diagnostic case still returns one candidate because
other retained states fail subsequent constraints. No live Valhalla comparison
was performed.

The ranking fixture reconstructs the complete bounded pool before its one-result
ranking limit. Both versions reconstruct three successful candidates. The new
implementation replaces three P2 Walking Route calls (one direct success and two
unreachable transit-edge alternatives) with three Walking Matrix calls. The direct
walk gains a reconstruction Route request, while the two transit candidates retain
their four access/egress Route requests. Thus total Route calls decrease from seven
to five, total Matrix calls increase from two to five, and reconstruction Route
calls increase from four to five. The two access/egress discovery matrices are
separate from P2 work. BikeShare is disabled in this fixture.

The direct-corridor regression compares transit-only and combined P2 evaluation;
its one additional direct Walking request changes from Route to Matrix. The direct
corridor still adds one expansion. Measurements cover both direct-first budget
handling and the full ranking pool; no insertion-order truncation is used.

Conservative hydration pruning can retain more distinct prefixes within the same
frontier cap, changing subsequent work. The 12-edge synthetic BikeShare regression
performs 57 distinct exact realizations and 86 cache hits, versus 72 realizations
and 66 hits before conservative hydration pruning. Its updated pass
uses 303 expansions and returns a bounded 16-candidate pool. Synthetic injection
makes no physical BikeShare Matrix calls. The concrete endpoint-session regression
separately verifies two Matrix calls (station access plus bicycle metrics), cached
anchor remapping without additional calls, one candidate discovery, and isolation
from another request's unavailable snapshot.

Walking cache tests verify successes, unreachable cells, and provider failures;
ordered coordinate/boarding keys and departure instants remain distinct. Repeated
identical requests use the cache across corridors. Matrix calls and reconstruction
Route calls must be counted separately in any live comparison.

New timing regressions cover an access Route that misses the early transit but
catches a later departure, transfer followed by egress through both anchors,
unreachable geometry without synthetic success, actual direct-walk metrics and
maximum duration, and recomputation of rental waits after exit-walk changes.

The stricter street-path retention can increase reconstruction work and cause
small budgets to finish fewer transit corridors. Incomplete corridors contribute
nothing; completed direct and transit corridors survive later exhaustion. All
frontier, expansion, deadline, station, and batching limits remain unchanged.

## Explicit expansion and actual-time feasibility

The expansion consolidation compares `d2eccbc` with the working-tree refactor.
Transit, Walking, and BikeShare now expand explicitly in that order, and all
feasible successors reach the existing shared Pareto frontier. Three former
movement modules, the dispatch trait, and the expansion context are removed.
Walking and BikeShare no longer screen candidates with optimistic duration bounds.
Candidate discovery still requires availability and a distinct pickup/dropoff pair.

Tight-query regressions expose the intentional extra work with deterministic
fixtures. Before counts follow the original screening and charging control flow;
after counts are asserted by the updated tests:

| Fixture work | Before | After |
| --- | ---: | ---: |
| Walking latest-arrival rejection: P2 Walking Matrix requests | 0 | 1 |
| Walking latest-arrival rejection: charged expansions | 2 | 3 |
| BikeShare hard-time rejection: synthetic exact realizations | 0 | 1 |
| BikeShare hard-time rejection: P2 Walking Matrix requests | 1 | 1 |
| BikeShare hard-time rejection: charged expansions | 3 | 4 |

`walking_replacement_respects_latest_arrival` rejects the actual 300-second walk
against a 299-second arrival window after one Matrix request.
`bike_actual_realization_enforces_query_time_limits` rejects the actual
120-second synthetic BikeShare movement against a 30-second maximum duration or
latest-arrival window. Its Walking alternative is also realized before rejection.
The BikeShare injection makes no physical bicycle Matrix calls. No usable station
pair still causes no BikeShare realization or charge. Budgets and limits are
unchanged; requests with tight time limits can therefore exhaust them earlier.

`walking_uses_actual_matrix_timing_even_when_geometry_bound_exceeds_query_limit`
uses deliberately synthetic Matrix timings to verify that only actual returned
timing controls acceptance, for both maximum duration and latest arrival.
Cache regressions preserve direction, coordinates, boarding identity, departure
instant, Taipei normalization, failed-result reuse, and request isolation.

The documented release routing/reconstruction benchmarks passed, as did the
release direct-corridor benchmark. The following warm binary runs each use
1,000 measured iterations, with no compilation running alongside the measurement.
These host-dependent fixture timings do not establish a live-provider speedup.

| Planner fixture | Before median (µs) | After median (µs) | P2 expansions, both | P2 Walking Matrix requests, both | Walking cache hits, both |
| --- | ---: | ---: | ---: | ---: | ---: |
| MRT | 19.291 | 21.666 | 7 | 2 | 0 |
| Bus | 14.584 | 14.250 | 6 | 2 | 0 |
| Bus → MRT | 34.125 | 33.834 | 10 | 3 | 0 |
| MRT → Bus | 50.500 | 48.000 | 29 | 8 | 2 |
| Multiple transfers | 96.626 | 93.209 | 37 | 8 | 2 |

BikeShare requests, cache hits, and Matrix calls remain zero in these fixtures.
Returned P2 candidate counts remain 3 / 2 / 3 / 1 / 9 respectively. The synthetic
12-edge BikeShare regression still performs 57 exact realizations and 86 cache
hits; the concrete session test still uses two Matrix requests and reuses both
candidate discovery and exact results across anchor-index remapping.

| Ranking fixture | Before median / p95 (µs) | After median / p95 (µs) |
| --- | ---: | ---: |
| Eligible transit | 33 / 36 | 32 / 35 |
| Rejected transit | 33 / 37 | 32 / 33 |

Ranking work remains nine P2 expansions, three P2 Walking Matrix requests,
two discovery matrices, five reconstruction Route requests, three reconstructed
candidates, and one ranked result. All cache and BikeShare work counts in that
fixture remain zero. Earlier runs adjacent to resource-heavy builds varied much
more (including 76 µs after-change ranking medians); the table reports warm runs
rather than attributing those host effects to the refactor.

The direct-corridor benchmark uses 100 iterations: transit-only P2 measured
3 / 4 µs median / p95, five expansions, and one Walking Matrix request; prepending
the direct corridor measured 5 / 5 µs, six expansions, and two Walking Matrix
requests. Its exact-budget checks still preserve completed corridors and discard
unfinished ones. No live Valhalla or TDX performance comparison was performed.
