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
`direct_corridor_has_observable_constant_work` compares transit-only and
combined P2 work over 100 iterations and checks state and Matrix work counts.
Run its release measurements with:

```bash
cargo test --manifest-path server/Cargo.toml --release --lib direct_corridor_has_observable_constant_work -- --nocapture
```

`ranking_considers_three_corridors_before_applying_a_one_result_limit` checks
that two earlier fallback-timed MRT routes cannot exclude a later preferred
walk, and that ranking rejection of both MRT routes still leaves the walk.
`all_corridors_complete_with_direct_walking_and_bike_share` checks that surface
results survive while all prepared corridors complete.
`deadline_remains_a_hard_failure_after_a_completed_corridor` uses paused time
to verify that partial-result handling never suppresses a deadline.

Run both release-mode routing and reconstruction benchmarks with:

```bash
cargo test --manifest-path server/Cargo.toml --release --test routing benchmark -- --ignored --nocapture --test-threads=1
```

The ignored representative benchmarks use the fixed artifact
`server/data/build/20261001T173300Z/.via-static.json` and Valhalla at
`http://valhalla:8002/`. They use static timing and fixture BikeShare
availability; they do not exercise live TDX. Run from `server/` when those
local prerequisites are available:

```bash
cargo test --release --locked --lib phase2_corridor_representative_benchmark -- --ignored --nocapture
cargo test --release --locked --lib capture_representative_pools -- --ignored --nocapture
```

These are fixed workloads, not current live-traffic measurements. The artifact
is not included in the repository.
