# Via configuration

[config/via.toml](../config/via.toml) is the canonical nonsecret configuration and complete reference for default values. All sections are required, except `server.static_path`, which can be omitted when `VIA_STATIC` is supplied. Unknown keys and invalid values fail startup; an explicit malformed file never falls back to defaults. Changes require a process restart.

Each executable loads the file at its entry point and injects typed settings into its dependencies. Providers never open the configuration file. Constructors and preprocessing functions require settings explicitly. Tests construct typed defaults from the same tracked TOML embedded at compile time, without reading files or using credentials. The application entry points always load the runtime file, so editing it does not require recompiling the binary.

## Paths and precedence

Configuration path precedence is `--config` (route/data CLI), then `VIA_CONFIG`, then `config/via.toml` under the server's compile-time manifest directory. Local discovery is independent of the current working directory. An explicit relative path is relative to the working directory. Installed binaries must supply `VIA_CONFIG=/absolute/path/via.toml`; source directories are not assumed to exist in deployment. Docker copies the file to `/etc/via/via.toml` and sets `VIA_CONFIG` accordingly. Mount a replacement file there to change container settings.

Setting precedence is explicit CLI override, then supported environment override, then TOML. Supported compatibility overrides are:

| Variable | Setting / purpose |
| --- | --- |
| `PORT` | `server.port` |
| `VIA_TDX_BASE_URL` | `tdx.api_base_url` |
| `VIA_VALHALLA_URL` | `valhalla.base_url` |
| `VIA_STATIC` | `server.static_path`; selects an existing artifact |
| `VIA_ENV` | Deployment selector: `development` or `production` |
| `TDX_CLIENT_ID`, `TDX_CLIENT_SECRET` | Secrets; never put these in TOML |
| `RUST_LOG` | Existing tracing filter |

The existing dotenv loading order is retained: process environment wins over discovered `.env`, then `server/.env`, then repository `.env`. Old nonsecret entries in `.env` override TOML; remove them when moving ownership to the file. Compose retains its network-specific Valhalla URL override. `VALHALLA_TILE_URL` configures the external Valhalla image rather than Via.

The server still requires explicit `VIA_ENV`. Both CLIs default to development when it is absent. Production requires HTTPS for the TDX API, OAuth token endpoint, and Valhalla. Development disables the existing overall planning and HTTP deadlines; provider timeouts remain active. TOML cannot disable production deadlines.

`via-route --max-static-corridors`, `--max-departures-per-query`, and `--valhalla-timeout-ms` override their corresponding settings. The existing timeout flag also sets the overall planning deadline to five times that timeout. Zero CLI limits fail validation instead of being silently clamped.

## Reference

The tracked file lists every key and its default in one place. Durations use the unit in the key; all counts and capacities are integers. Transfer count and rate-limit retries may be zero. Other resource limits must be positive.

| Section | Keys and units | Effect |
| --- | --- | --- |
| `server` | `port`, optional `static_path`, `request_timeout_ms`, `http_timeout_ms`, `max_concurrent_requests`, `max_body_bytes` | TCP binding, planning and HTTP deadlines, admission capacity and request body limit |
| `routing` | `max_transfers`, `max_static_corridors`, `max_concurrent_phase2_states`, `max_concurrent_phase2_corridors`, `max_concurrent_reconstruction_candidates`, `max_candidates`, `max_departures_per_query`, `max_journey_duration_seconds` | Corridor search cap, concurrency and routing bounds; `max_candidates` controls both the per-edge frontier and completed-candidate bound |
| `routing.access` | `max_transit_access_stops`, `max_transit_egress_stops`, `max_walking_radius_metres`, `max_walking_seconds` | Access/egress discovery and walking checks |
| `routing.ranking` | `metro_transfer_buffer_seconds`, `bus_transfer_buffer_seconds`, `near_duplicate_arrival_seconds`, `material_walking_difference_seconds`, `max_sequence_only_option_delay_seconds`, `reliability_preference_window_seconds`, `result_limit` | Ranking thresholds and output count, bounded by routing candidate capacity; the transfer buffers also weight transfers in corridor search |
| `bikeshare` | `max_pickup_stations_per_anchor`, `max_dropoff_stations_per_anchor`, `max_stations_per_endpoint`, `endpoint_radius_metres`, `max_endpoint_walk_seconds`, `max_bike_duration_seconds`, `max_bike_distance_metres`, `optimistic_walk_speed_metres_per_second`, `rental_handling_seconds` | Shared snapshot/endpoint discovery radius, candidate counts and movement bounds; `rental_handling_seconds` is added to every ride for unlocking and docking (about 30 s each for YouBike 2.0, an estimate), so very short rentals do not beat walking; endpoint walking bounds apply to exact endpoint walks, while retained boarding-point access uses the artifact preprocessing bounds |
| `bikeshare.preprocessing` | `max_geometric_radius_metres`, `max_walking_duration_seconds`, `max_walking_distance_metres`, `max_geometric_candidates_per_boarding_point`, `max_access_stations_per_boarding_point`, `max_matrix_concurrency` | Build-time station access policy; geometric candidates must fit both Matrix dimensions and cover retained access count |
| `preprocessing` | `same_stop_transfer_seconds` | Build-time same-stop transit transfer buffer embedded in the artifact |
| `preprocessing.transfers` | `radius_metres`, `max_duration_seconds`, `max_distance_metres`, `max_outgoing`, `max_concurrency`, `source_group_size` | Build-time walking transfers and bounded preprocessing work |
| `tdx` | `api_base_url`, `token_url`, `connect_timeout_ms`, `request_timeout_ms`, `download_timeout_seconds`, `max_response_bytes`, `max_rate_limit_retries`, `minimum_token_lifetime_seconds`, `token_refresh_margin_seconds`, `bike_catalog_page_size` | HTTP/OAuth resources, existing Retry-After behavior and static station catalog fetch pagination |
| `tdx.batch` | `bike_stations`, `bus_stops`, `metro_pairs`, `metro_response_limit` | City-scoped BikeShare/Bus batches and unique Metro line/station pairs; no TRA batch cap |
| `tdx.realtime` | `fresh_seconds`, `degraded_seconds`, `max_clock_skew_seconds`, `cache_ttl_seconds`, `cache_max_bytes` | Shared Bus/Metro/TRA timing freshness and future-timestamp tolerance; Bus/Metro/TRA live responses are reused across requests by exact TDX URL for `cache_ttl_seconds` (less than `fresh_seconds`), up to `cache_max_bytes` |
| `tdx.bikeshare` | `cache_ttl_seconds`, `cache_capacity`, `max_concurrent_cities`, `max_age_seconds`, `max_clock_skew_seconds` | Availability cache, city concurrency and shared provider/planner freshness checks |
| `valhalla` | `base_url`, `request_timeout_ms`, `build_request_timeout_ms`, `max_concurrency`, `max_sources`, `max_targets`, `max_response_bytes` | Runtime/build provider limits and Matrix capacity |

BikeShare batching is city-specific for every supported city and `$top` equals the requested station count.

Metro retains its response ceiling. A response reaching `metro_response_limit` is potentially truncated and is rejected through the existing provider-error/static-fallback path. No pagination or compensating retries are added. This can increase fallback for dense observations. If upstream truncates below the requested ceiling without metadata, completeness cannot be established from the current endpoint response; investigate endpoint pagination before raising the ceiling.

## Use and validation

```sh
cargo run --manifest-path server/Cargo.toml --bin via-data -- validate-config
cargo run --manifest-path server/Cargo.toml --bin via-data -- --config /path/via.toml validate-config
VIA_CONFIG=/path/via.toml cargo run --manifest-path server/Cargo.toml --bin via-server
cargo run --manifest-path server/Cargo.toml --bin via-data -- --config /path/via.toml build --input /path/snapshot
```

Configuration validation needs neither TDX credentials nor a running Valhalla service. The data `validate` and `inspect` subcommands likewise remain offline. Fetching requires credentials; building requires Valhalla. `via-route` loads the same file and requires the existing static artifact and live dependencies.

Edit `routing` to tune search budgets, `routing.access` to tune access, and `tdx.batch` to tune batching. Changes to `bikeshare.preprocessing` or `preprocessing` only affect newly built artifacts: run `via-data build`, validate the artifact, then select it with `VIA_STATIC` and restart the server. Existing artifacts are never rebuilt automatically. `valhalla.build_request_timeout_ms` applies only to preprocessing.

## Constants retained locally

- Physical/mathematical constants, geographic index precision padding, unit conversions and encoded-polyline precision.
- Protocol paths, OAuth grant fields, TDX city/ID formats, status codes, schema and canonical identity versions, attribution/license URLs and service-day/timezone conventions.
- The provider rental reuse rule (15 minutes), preserved consistently in evaluation and reconstruction as a domain constraint.
- Fixed diagnostic sample counts, hashing/read buffer sizes and CLI inspection display defaults, which do not affect routing or provider resource policy.
- Domain-only compatibility defaults for `RankingPolicy`, `FreshnessPolicy`, BikeShare availability views and offline/fake Matrix routers. Production injects TOML policies; tests assert canonical ranking defaults still agree. No domain module depends on application configuration.
