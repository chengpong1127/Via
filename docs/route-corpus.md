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
| YouBike surface transfer | `bike_can_realize_a_surface_transfer_without_skipping_transit` | Transit anchors remain fixed and transfer can use a bike |
| Bike access and return walks | `faster_bike_surface_transfer_catches_the_next_transit_departure` | Station access/return walking time is counted; bike catches the 10:06 departure |
| Bike availability | `no_bike_at_pickup_keeps_the_walking_transfer`, `no_dock_at_dropoff_keeps_the_walking_transfer` | Walking remains when either station condition fails |
| Bike candidate bound | `station_choices_are_bounded_to_three_nearest_per_anchor` | At most three pickup and three return stations are considered per anchor |
| Direct walking | `direct_walking_does_not_gain_a_bike_share_variant` | Direct walking remains walking-only |

For the standard checks, run from the repository root:

```bash
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features
cargo test --manifest-path server/Cargo.toml --all
```

See [`dev-tooling.md`](dev-tooling.md) for deterministic CLI routes and live
provider configuration.
