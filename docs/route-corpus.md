# Via route tests

The deterministic route corpus is implemented in `server/tests/routing` and
uses provider-neutral network, timing, and street fixtures. It does not need
TDX, Valhalla, or network access.

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
| Cross-anchor YouBike | `one_bike_shortcut_can_skip_multiple_transit_anchors` | A bounded BikeShare shortcut can replace multiple structural transit legs as one rental |
| Walking replacement | `bike_can_replace_a_surface_walking_edge` | A BikeShare realization is considered for a retained walking edge |
| Pareto usefulness | `bike_with_no_pareto_benefit_is_pruned`, `transit_remains_when_it_reaches_the_anchor_before_bike` | Slower bike-only variants are pruned while faster transit survives |
| Pareto tradeoff | `pareto_keeps_a_later_route_with_less_walking` | Earlier high-walking and later low-walking candidates both remain |
| Bike access and return walks | `faster_bike_surface_transfer_catches_the_next_transit_departure` | Station access and return walking are counted |
| Bike availability | `no_bike_at_pickup_keeps_the_walking_transfer`, `no_dock_at_dropoff_keeps_the_walking_transfer` | Bike use requires an available pickup bike and return dock |
| Bike candidate bounds | `station_choices_are_bounded_to_three_nearest_per_anchor`, `corridor_movements_include_bounded_forward_anchor_pairs` | Endpoint choices are top three and forward shortcuts span at most eight anchors |
| Direct walking | `direct_walking_gains_a_bike_share_variant_with_the_shared_snapshot` | A direct BikeShare candidate is generated from the shared availability snapshot |

For the standard checks, run from the repository root:

```bash
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features
cargo test --manifest-path server/Cargo.toml --all
```

See [`dev-tooling.md`](dev-tooling.md) for deterministic CLI routes and live
provider configuration.
