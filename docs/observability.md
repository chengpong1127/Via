# Runtime observability

Runtime operational telemetry is recorded directly through the `metrics` crate
at the component that owns the measurement. `server/src/metrics.rs` installs the
Prometheus recorder, configures histogram buckets, and runs recorder upkeep.
Histograms use seconds. There are no dynamic stop, pattern, request, or location
labels.

## Operational metric inventory

All names below are production metrics. Counters end in `_total`; the two
`_duration_seconds` metrics are histograms.

| Metric | Owner and measurement |
| --- | --- |
| `via_planning_requests_total` | HTTP handler: one completed planning attempt after validation |
| `via_planning_empty_total` | HTTP handler: successful outcome with no options |
| `via_planning_options_total` | HTTP handler: number of returned options |
| `via_planning_failures_total` | HTTP handler: failed planning attempts |
| `via_planner_transit_candidate_stops_total` | RoutingService: combined access/egress count after discovery; zero if either discovery fails |
| `via_planner_candidate_discovery_duration_seconds` | RoutingService: completed access/egress discovery, including provider errors, after deadline checks |
| `via_planner_multimodal_search_duration_seconds` | RoutingService: successful prepare/evaluate/reconstruct operation, including fallback and reconstruction |
| `via_planner_transit_reachable_boardings_total` | MultimodalPlanner Phase 1: retained timing positions |
| `via_planner_transit_reachable_patterns_total` | MultimodalPlanner Phase 1: retained patterns |
| `via_planner_transit_realtime_stops_requested_total` | MultimodalPlanner Phase 1: unique retained Bus stops; preserves the historical static selection count, not an HTTP request count |
| `via_planner_transit_departure_lookups_total` | PlanningSession evaluation boundary: lookup delta from each successfully completed evaluation pass |
| `via_planner_transit_realtime_evidence_values_total` | RoutingService: evidence in journeys returned by evaluate_primary, after its deadline check and before reconstruction |
| `via_bus_timing_logical_departure_needs_total` | Bus timing provider: active logical departure queries |
| `via_bus_timing_realtime_stops_requested_total` | Bus timing provider: unique stops selected for realtime fetch |
| `via_bus_timing_realtime_batches_total` | Bus timing provider: planned realtime batches |
| `via_bus_timing_realtime_rows_received_total` | Bus timing provider: received realtime rows |
| `via_bus_timing_realtime_rows_relevant_total` | Bus timing provider: locally relevant realtime rows |
| `via_bus_timing_realtime_http_requests_total` | Bus timing provider: attempted upstream HTTP requests |
| `via_bus_timing_upstream_failures_total` | Bus timing provider: failed upstream batches |
| `via_bus_timing_rate_limited_requests_total` | Bus timing provider: upstream failures due to rate limiting |
| `via_bike_availability_stations_requested_total` | Bike availability provider: unique station IDs requested, including unsupported identities |
| `via_bike_availability_station_cache_hits_total` | Bike availability provider: fresh per-station cache hits before city refresh |
| `via_bike_availability_stations_fetched_total` | Bike availability provider: station IDs in attempted upstream batches |
| `via_bike_availability_http_requests_total` | Bike availability provider: attempted filtered batches, excluding internal HTTP retries |

## Completion and failure semantics

The metrics cleanup preserves names, units, HTTP outcome semantics, Bus provider
semantics, discovery error normalization, and the successful multimodal duration
scope (including reconstruction). Invalid HTTP requests and cancelled handler
futures do not increment the completed-request counter.

Stage telemetry now survives a later stage's failure or request cancellation.
Previously `PlannerStageMetrics` delayed emission until finalization, so a later
deadline could erase already completed work. Phase 1 counts now appear even when
snapshot loading or evaluation later fails. A completed evaluation pass is counted
even when another pass fails. Incomplete/failed evaluation passes do not emit a
lookup delta. Discovery interrupted by its deadline checks still emits no sample.
The multimodal duration still records only a successfully completed operation.

Evidence scope is unchanged: `evaluate_primary()` can automatically use the
surface fallback when no transit journey is realized. Its returned collection is
counted. A separate fallback triggered by failed reconstruction is excluded.
Evaluation lookup deltas prevent counting cumulative session stats again during
fallback. Direct planner callers, including `plan_with_stats()`, now record the
planner stages they execute. Skipped stages need not register zero-valued series;
Prometheus series are registered when their instrumentation runs.

## Structured accounting

`MultimodalPlannerStats` and `plan_with_stats()` remain benchmark/debug APIs.
`StaticSearchStats` retains only expansion count, unique/retained corridor counts,
retained pattern/position/Bus-stop counts, total search latency, Kth duration,
and search-limit, early-termination, and deadline status. Pattern, position, and
Bus-stop counts preserve the timing snapshot and Prometheus accounting.
Phase 1 emits one compact search summary and traces the retained corridors.

`Phase2EvaluationStats` retains the cumulative expansion count and departure
lookups, BikeShare lower-bound checks/rejections, exact requests/cache hits/matrix
requests, final candidate count, and total evaluation latency. The expansion
count enforces the session's safety bound across primary and fallback evaluation.
Phase 2 emits one compact evaluation summary. `SurfaceRealizationStats` and
`BikePlanningStats` retain exact routing, cache, and matrix work accounting;
snapshot latency remains in tracing.

`VIA_ENV=development` disables wall-clock planning and HTTP timeouts for
debugging. `VIA_ENV=production` retains the normal planning timeout (5 seconds
by default) and HTTP timeout (10 seconds). Both modes retain all algorithm
expansion, candidate, corridor, transfer, and journey-duration limits.

`TransferStatistics`, `BikeAccessDiagnostics`, `GtfsBuildDiagnostics`, and
`BuildDiagnostics` remain returned offline build output. `PathMetrics`,
`WalkingTransferMetrics`, and `StreetMetric` describe actual routing values.
Test fixture statistics and CLI `StatsArgs` are also unrelated to operational
telemetry. These types are not Prometheus staging objects. Bus
`RealtimeDepartureResults` retains provider accounting used by its logic/tracing.

`PlannerStageMetrics`, `PlanningCandidates.metrics`, the `finalize_plan` metrics
argument, and service-level planner stats copying were deleted. There is no
operational telemetry accumulator carried to finalization.
