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
| Pareto usefulness | `bike_with_no_pareto_benefit_is_pruned`, `transit_remains_when_it_reaches_the_anchor_before_bike` | Slower bike-only journeys are pruned while faster transit survives |
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
keeps an already-routed slower walk when transit geometry fails.
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

A sequential run of the pre-refactor and updated release binaries, with compiler
work paused during measurement, produced these in-memory results over 1,000
iterations. Timings are host-dependent and do not measure live Valhalla latency.

| Fixture / work | Before | After |
| --- | --- | --- |
| Ranking fixture, reconstructed journeys per request | 1 | 3 |
| Ranking fixture, route calls | 5 | 7 |
| Ranking fixture, matrix calls | 2 | 2 |
| Ranking fixture, hydration route calls | 2 | 4 |
| Ranking fixture, eligible-transit median / p95 | 31 / 35 µs | 32 / 35 µs |
| Ranking fixture, rejected-transit median / p95 | 25 / 28 µs | 32 / 36 µs |
| MRT planner median | 17.917 µs | 14.376 µs |
| Bus planner median | 13.667 µs | 12.959 µs |
| Bus → MRT planner median | 33.751 µs | 28.750 µs |
| MRT → Bus planner median | 39.750 µs | 38.667 µs |
| Multiple-transfer planner median | 36.584 µs | 35.292 µs |
| P2 expansions, same five planner fixtures | 5 / 5 / 8 / 11 / 11 | 5 / 5 / 8 / 11 / 11 |
| Repeated Walking realization, physical requests for the same departure | 1 | 1 |
| 12-edge BikeShare fixture, exact realizations / cache hits | 72 / 66 | 72 / 66 |

The two additional reconstruction routes in the ranking fixture hydrate the second
MRT candidate's access and egress geometry. All three candidates reach ranking;
the reliable direct walk wins the one-result limit, or remains eligible when both
MRT candidates are rejected. The previous implementation selected the first MRT
candidate or returned an empty result after its rejection. The fixture has no
BikeShare, so its BikeShare matrix calls and cache hits remain zero. The Walking
cache regression also checks a distinct departure time: three realizations make
two physical requests and record one cache hit. These counters demonstrate the
work changes independently of timing noise.
