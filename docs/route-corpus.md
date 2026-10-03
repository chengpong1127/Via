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
| Walking fallback | `direct_walking_is_returned_when_no_transit_access_is_reachable` | When transit has no usable access, `evaluate_fallback()` builds a surface-only Origin-to-Destination corridor and returns its feasible walking movement |
| BikeShare fallback | `direct_walking_without_a_transit_query_can_use_bike_share` | The surface-only fallback corridor can realize BikeShare with the request-local availability snapshot |

For the standard checks, run from the repository root:

```bash
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path server/Cargo.toml --all-targets --all-features
git diff --check
```

See [`dev-tooling.md`](dev-tooling.md) for deterministic CLI routes and live
provider configuration.
