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
| `VIA_TDX_GATEWAY_URL` | `tdx.gateway.url`; the server reads live data from `via-tdx-gateway` and needs no TDX credentials |
| `VIA_STATIC` | `server.static_path`; selects an existing artifact |
| `VIA_ENV` | Deployment selector: `development` or `production` |
| `TDX_CLIENT_ID`, `TDX_CLIENT_SECRET` | Secrets; never put these in TOML |
| `RUST_LOG` | Existing tracing filter. Never enable `axum::rejection=trace` (or a global `trace`): rejection events can include request query values such as coordinates, which the server must not log |

The existing dotenv loading order is retained: process environment wins over discovered `.env`, then `server/.env`, then repository `.env`. Old nonsecret entries in `.env` override TOML; remove them when moving ownership to the file. Compose retains its network-specific Valhalla URL override. `VALHALLA_TILE_URL` configures the external Valhalla image rather than Via.

`via-tdx-gateway` is the only process that needs TDX credentials when `tdx.gateway.url` is set. It owns the OAuth token, the live cache and the rate-limit backoff, and serves `POST /live/bus/arrivals`, `/live/bike/availability`, `/live/metro/boards` and `/live/tra/delays` plus `/health/live`, `/health/ready` and `/metrics`. Run exactly one replica so the cache and the TDX quota stay in one place. Without `tdx.gateway.url` the server keeps the same cache in-process and calls TDX directly. The gateway requires explicit `VIA_ENV` and ignores `tdx.gateway.url` and `VIA_STATIC`. It logs one `live data request` line per call and exports `via_tdx_gateway_requests_total`, `via_tdx_gateway_request_seconds`, `via_live_entities_requested_total`, `via_live_upstream_requests_total`, `via_live_entities_unavailable_total` and `via_live_rate_limit_blocks_total`, labelled by dataset, plus `via_tdx_upstream_requests_total` (dataset, status) and `via_tdx_response_bytes_total` (dataset) for TDX usage.

The server still requires explicit `VIA_ENV`. Both CLIs default to development when it is absent. Production requires HTTPS for the TDX API and OAuth token endpoint. Valhalla and `tdx.gateway.url` also require HTTPS unless the host is internal: a loopback or private IP address, a single-label name such as `valhalla`, or a name ending in `.svc` or `.svc.cluster.local`. Development disables the existing overall planning and HTTP deadlines; provider timeouts remain active. TOML cannot disable production deadlines.

`via-route --max-static-corridors`, `--max-departures-per-query`, and `--valhalla-timeout-ms` override their corresponding settings. The existing timeout flag also sets the overall planning deadline to five times that timeout. Zero CLI limits fail validation instead of being silently clamped.

## Reference

The tracked file lists every key and its default in one place. Durations use the unit in the key; all counts and capacities are integers. Transfer count and rate-limit retries may be zero. Other resource limits must be positive.

| Section | Keys and units | Effect |
| --- | --- | --- |
| `server` | `port`, optional `static_path`, `request_timeout_ms`, `http_timeout_ms`, `max_concurrent_requests`, `max_body_bytes` | TCP binding, planning and HTTP deadlines, admission capacity and request body limit |
| `routing` | `max_transfers`, `max_static_corridors`, `max_concurrent_phase2_states`, `max_concurrent_phase2_corridors`, `max_concurrent_reconstruction_candidates`, `max_candidates`, `max_departures_per_query`, `max_journey_duration_seconds` | Corridor search cap, concurrency and routing bounds; `max_candidates` controls both the per-edge frontier and completed-candidate bound |
| `routing.access` | `max_transit_endpoint_stops`, `max_walking_radius_metres`, `max_walking_seconds` | Access/egress discovery and walking checks |
| `routing.ranking` | `metro_transfer_buffer_seconds`, `bus_transfer_buffer_seconds`, `material_walking_difference_seconds`, `max_sequence_only_option_delay_seconds`, `reliability_preference_window_seconds`, `result_limit` | Ranking thresholds and output count; `result_limit` defaults to 5, may not exceed 5, and is also bounded by routing candidate capacity; the transfer buffers also weight transfers in corridor search |
| `bikeshare` | `max_pickup_stations_per_anchor`, `max_dropoff_stations_per_anchor`, `max_stations_per_endpoint`, `endpoint_radius_metres`, `max_endpoint_walk_seconds`, `max_bike_duration_seconds`, `max_bike_distance_metres`, `optimistic_walk_speed_metres_per_second`, `rental_handling_seconds` | Shared snapshot/endpoint discovery radius, candidate counts and movement bounds; `rental_handling_seconds` is added to every ride for unlocking and docking (about 30 s each for YouBike 2.0, an estimate), so very short rentals do not beat walking; endpoint walking bounds apply to exact endpoint walks, while retained boarding-point access uses the artifact preprocessing bounds |
| `preprocessing` | `same_stop_transfer_seconds` | Build-time same-stop transit transfer buffer embedded in the artifact |
| `preprocessing` | `bus_derived_segment_scale_percent` | Build-time scale (percent, default 290, 100 = unchanged, must be > 0) for Bus segment times that are not timetable data (`derived` from GTFS run times, `estimated`); `schedule` segments are never scaled. Calibrate against TDX `Bus/RealTimeNearStop` vehicle progress (elapsed time since trip start vs the artifact segment sum) |
| `preprocessing.transfers` | `radius_metres`, `max_duration_seconds`, `max_distance_metres`, `max_outgoing`, `max_concurrency`, `source_group_size` | Build-time walking transfers and bounded preprocessing work |
| `preprocessing.bike_access` | `max_geometric_radius_metres`, `max_walking_duration_seconds`, `max_walking_distance_metres`, `max_geometric_candidates_per_boarding_point`, `max_access_stations_per_boarding_point`, `max_matrix_concurrency` | Build-time station access policy; geometric candidates must fit both Matrix dimensions and cover retained access count |
| `tdx` | `api_base_url`, `token_url`, `connect_timeout_ms`, `request_timeout_ms`, `download_timeout_seconds`, `max_response_bytes`, `max_rate_limit_retries`, `minimum_token_lifetime_seconds`, `token_refresh_margin_seconds`, `bike_catalog_page_size`, `rate_limit_backoff_seconds` | HTTP/OAuth resources, existing Retry-After behavior and static station catalog fetch pagination; after a live-data 429, no further live TDX call is made for the `Retry-After` duration (capped at 60 s), or when the header is absent for `rate_limit_backoff_seconds` (default 3 s, the first step), doubling on each consecutive failure up to 60 s and reset by a successful call. Repeated 5xx or unexpected 4xx responses use the same pause. A 401 drops the cached token and retries once |
| `tdx.fetch` | `requests_per_minute`, `connect_timeout_ms`, `request_timeout_ms`, `max_attempts`, `retry_backoff_seconds` | `via-data fetch` only; live server and gateway keep the `tdx` timeouts. Every fetch call (GTFS download, Bike catalog pages, Bus routes) shares one limiter at `requests_per_minute` (default 4, the free-tier pace: the interval is 60 / `requests_per_minute` seconds and it must stay below the per-key request limit of your TDX plan, which the gateway shares; see https://tdx.transportdata.tw/pricing, and raise it only after the owner has upgraded the plan). `connect_timeout_ms` (default 5000) also applies to the GTFS download so slow DNS does not abort it. Transient failures (timeouts, 5xx, 429) are retried up to `max_attempts` with `retry_backoff_seconds` doubling per retry, or the `Retry-After` value on 429; the GTFS download resumes with a Range request. |
| `tdx.batch` | `bike_stations`, `bus_stops`, `metro_pairs`, `metro_response_limit` | City-scoped BikeShare/Bus batches and unique Metro line/station pairs; no TRA batch cap |
| `tdx.realtime` | `fresh_seconds`, `degraded_seconds`, `max_clock_skew_seconds`, `cache_ttl_seconds`, `cache_max_bytes` | Shared Bus/Metro/TRA timing freshness and future-timestamp tolerance; Bus/Metro/TRA live records are reused across requests per Bus stop and scope, Metro line/station pair, or TRA train number for `cache_ttl_seconds` (less than `fresh_seconds`), up to `cache_max_bytes`; an entity with no records is cached as empty |
| `tdx.bikeshare` | `cache_ttl_seconds`, `cache_capacity`, `max_concurrent_cities`, `max_age_seconds`, `max_clock_skew_seconds` | Per-station availability cache, city concurrency and shared provider/planner freshness checks |
| `tdx.gateway` | `url` (optional), `port`, `max_ids_per_request` | `url` selects gateway mode for the server and `via-route`; `port` and `max_ids_per_request` configure the `via-tdx-gateway` listener |
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

Edit `routing` to tune search budgets, `routing.access` to tune access, and `tdx.batch` to tune batching. Changes to `preprocessing` (including `preprocessing.bike_access`) only affect newly built artifacts: run `via-data build`, validate the artifact, then select it with `VIA_STATIC` and restart the server. Existing artifacts are never rebuilt automatically. `valhalla.build_request_timeout_ms` applies only to preprocessing.

## Constants retained locally

- Physical/mathematical constants, geographic index precision padding, unit conversions and encoded-polyline precision.
- Protocol paths, OAuth grant fields, TDX city/ID formats, status codes, schema and canonical identity versions, attribution/license URLs and service-day/timezone conventions.
- The provider rental reuse rule (15 minutes), preserved consistently in evaluation and reconstruction as a domain constraint.
- Fixed diagnostic sample counts, hashing/read buffer sizes and CLI inspection display defaults, which do not affect routing or provider resource policy.
- Domain-only compatibility defaults for `RankingPolicy`, `FreshnessPolicy`, BikeShare availability views and offline/fake Matrix routers. Production injects TOML policies; tests assert canonical ranking defaults still agree. No domain module depends on application configuration.

## Resilient and weekly fetch

`via-data fetch` writes each snapshot to a temporary directory and only renames it into `server/data/fetch/<timestamp>/` when complete; the CLI repoints `latest` afterwards. The GTFS archive is downloaded to `gtfs.zip.part` and renamed once it validates. If a Bike station or Bus route file still fails after its retries, the file from the snapshot `latest` points to is reused and listed in `manifest.json` under `reused_sources` (source name to the fetch time of the snapshot it came from). Without a previous snapshot the fetch fails and nothing is published. GTFS failures always fail the fetch.

`via-data fetch --gtfs-only` downloads only GTFS (one TDX call) and reuses every Bike, Bus route and Bus schedule file from `latest` (a previous snapshot without `bus-schedule-*` files is fine: those cities simply get no headway estimates); it fails if there is no previous snapshot. Use it for the weekly refresh.

Resume safety: the GTFS download stores the response `ETag` (or `Last-Modified`) next to the partial file and resumes with `Range` plus `If-Range`; it appends only when the server answers `206` with a `Content-Range` starting at the requested offset, and otherwise restarts from byte 0, so a regenerated archive is never spliced. Without a stored validator it restarts. Only transient errors (timeouts, 5xx, 429) fall back to the previous snapshot's copy of a catalog; permanent 4xx, oversized or invalid responses fail the fetch. `reused_sources` keeps the time the file was originally fetched across chained fallbacks, and `via-data fetch` prints a one-line summary of reused files. At the start of each fetch, leftover `.<timestamp>.<id>.tmp` directories under `server/data/fetch/` are deleted, so run only one fetch at a time. The OAuth token POST and the single restart after an HTTP 416 or a mismatched `206` are outside the request pacer: the token is cached and the restart replaces a request that already consumed its slot, so a download uses at most one extra call.

Bus headway estimates: `via-data fetch` also fetches Bus/Schedule for eight cities under the same paced policy (`requests_per_minute`, retries, reuse of the previous file on transient failure), so a full fetch adds eight paced requests. Artifacts built with them serialize `headway_seconds_by_period` on Bus bindings. Older servers reject that field (`deny_unknown_fields`): deploy the server first, then publish or select the new artifact. Artifacts without estimates load on both.
