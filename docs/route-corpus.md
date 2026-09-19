# Via route corpus

This is the small deterministic corpus used when changing the Trip-Based
search or its immutable indexes. Static fixture assertions use the existing
`server/tests/routing/fixtures.rs` network and schedules; live TDX runs should
assert route structure rather than exact arrival times.

| Case | Fixture or real route | Structural assertion |
| --- | --- | --- |
| MRT-only | `simple_mrt_uses_the_fixture_schedule_end_to_end` | one metro leg, zero transfers |
| Bus-only | `simple_bus_uses_the_earliest_realtime_departure` | one bus leg, fresh realtime evidence |
| Bus → MRT | `bus_to_mrt_transfer_keeps_the_bus_then_metro_order` | service sequence and one transfer |
| MRT → Bus | `mrt_to_bus_transfer_keeps_the_metro_then_bus_order` | service sequence and one transfer |
| Bus → Bus, same stop | `same_stop_transfer_uses_default_transfer_seconds` | default transfer time is required |
| Bus → Bus, walking | `walking_transfer_shortcut_expands_a_second_service` | walking transfer is retained |
| Multi-transfer | `three_transit_services_reconstruct_with_two_transfers` | ordered three-service sequence |
| TRA → MRT | `train_can_transfer_to_local_mrt` | rail then metro when fixture data supports it |

The selected-mode checks are also part of the fixture corpus:

| Selected modes | Fixture assertion |
| --- | --- |
| `bike_share` | `bike_share_only_uses_the_direct_walking_skeleton` |
| `metro` | `simple_mrt_uses_the_fixture_schedule_end_to_end` |
| `metro`, `bike_share` | `bike_variant_reconstructs_with_station_legs_and_bicycle_geometry` |
| `bus`, `metro`, `tra`, `thsr`, `bike_share` | `bike_connector_becomes_the_transfer_between_retained_transit_legs` |

The station-leg case replays the same request as `bike_share`, `metro` and
asserts the same stable matching key, covering selected-mode reordering.
The offline `via-route` tool rejects `bike_share`; production HTTP assembly
uses the explicit BikeShare artifact and availability source instead.

The real artifact smoke routes are:

```bash
cargo run --manifest-path server/Cargo.toml --quiet --bin via-route -- \
  --network server/data/dev/taipei/transit-network.json \
  --from-lat 25.0478 --from-lon 121.5170 \
  --to-lat 25.0339 --to-lon 121.5645 \
  --depart-at 2026-09-15T07:55:00+08:00 \
  --modes bus,metro,tra,thsr --service-conditions fixture --trace

cargo run --manifest-path server/Cargo.toml --quiet --bin via-route -- \
  --network server/data/dev/taipei/transit-network.json \
  --from-lat 25.0143 --from-lon 121.4637 \
  --to-lat 25.0339 --to-lon 121.5645 \
  --depart-at 2026-09-15T07:55:00+08:00 \
  --modes bus,metro,tra,thsr --service-conditions fixture --trace
```

For a timing comparison, warm the binary first and run each case repeatedly:

```bash
time cargo run --manifest-path server/Cargo.toml --quiet --bin via-route -- \
  --network server/data/dev/taipei/transit-network.json \
  --from-lat 25.0478 --from-lon 121.5170 \
  --to-lat 25.0339 --to-lon 121.5645 \
  --depart-at 2026-09-15T07:55:00+08:00 \
  --modes bus,metro,tra,thsr --service-conditions fixture --json >/dev/null
```

Compare median and p95 wall time, route existence, service sequence, transfer
count, walking seconds, and the Trip-Based debug diagnostics. The default
`max_departures_per_query = 3` remains the control value; a 1/2/3 departure
comparison has not justified changing it yet.

The controlled transfer-radius build comparison used the same raw snapshot,
Valhalla graph, and pruning policy:

| Metric | 500 m | 300 m |
| --- | ---: | ---: |
| Geometric candidate pairs | 963,523 | 928,456 |
| Matrix requests | 61,924 | 61,873 |
| Reachable walking pairs | 949,633 | 922,414 |
| Inferred edges kept | 339,795 | 325,294 |
| Total transfer edges | 339,821 | 325,320 |
| Build wall time | 630.0 s | 291.5 s |
| Outgoing fan-out average | 6.24 | 6.32 |
| Outgoing fan-out p50 / p95 / max | 6 / 12 / 13 | 6 / 12 / 13 |

Both Taipei Main → Taipei 101 and Banqiao → Taipei 101 produced the same
fixture smoke result under both artifacts: one walking option, with no transit
legs available from the fixture realtime setup. The canonical artifact remains
the 500 m build; 300 m is retained as a measured alternative rather than a
silent policy change.

Five direct-binary repetitions per route/artifact measured these wall-time
samples (milliseconds; p95 is the largest sample in this five-run set):

| Artifact / route | Median | p95 | Result |
| --- | ---: | ---: | --- |
| 500 m / Taipei Main | 8,747 | 8,979 | 1 walking option |
| 500 m / Banqiao | 8,291 | 8,377 | 1 walking option |
| 300 m / Taipei Main | 11,347 | 11,810 | 1 walking option |
| 300 m / Banqiao | 10,085 | 11,646 | 1 walking option |

These process-per-run smoke timings are noisy and include artifact startup;
they are recorded for comparison, not treated as a causal runtime result.

The departure-count control was run against the 500 m artifact with
`--max-departures-per-query` set to 1, 2, and 3. Both routes returned one
walking option in all cases, and the diagnostics were identical within each
route: Taipei Main had 4,876 TripStates and 175,036 unique departure queries;
Banqiao had 4,769 TripStates and 171,379 unique departure queries. Search
latencies were 6.26 / 6.44 / 6.20 s for Taipei Main and 6.54 / 6.42 / 6.08 s
for Banqiao (1 / 2 / 3 departures). Because this fixture run produced no
transit journey and does not exercise missed-departure recovery, the default
remains 3 rather than making an unsafe reduction from this limited corpus.
