# Via current architecture

Via plans multimodal journeys in Taipei and New Taipei using an immutable
GTFS-built transit network, request-local TDX realtime, Valhalla street
routing, and provider-neutral API models.

## Request flow

```text
PlanningRequest
  → RoutingService.plan()
       ├─ discover transit access / egress when transit is selected
       ├─ MultimodalPlanner.prepare() → PlanningSession.evaluate()
       │    ├─ Phase 1: bounded static Top-K transit corridors
       │    ├─ load request-local snapshots for retained transit positions
       │    ├─ convert retained corridors and prepend a direct walking EvaluationCorridor
       │    └─ Phase 2: fixed-corridor movement evaluation
       ├─ JourneyReconstructor assembles candidates
       └─ validate failures → rank → PlanningOutcome
```

`ViaRuntime` is the shared concrete application assembly used by both the HTTP
server and `via-route`. `RoutingService` owns the complete planning use case.
Its request-scoped `JourneyPlanningContext` and `JourneyPlanningLimits` live in
`routing/model.rs`.

Planner construction requires street routing, a BikeShare network and availability
source, and a timing resolver. Production runtime and Bus/Metro/TRA timing sources
require a TDX client; failed realtime calls still fall back to static timing.
Every `PlanningQuery` requires validated origin/destination coordinates and an
explicit deadline. Tests supply these dependencies and bounded deadlines; static
timing fixtures use a failing realtime client instead of an absent capability.
BikeShare selection and its request-local availability snapshot remain conditional.
Retained corridor conversion and reconstruction report broken internal references
as planning errors rather than dropping corridors or emitting empty station IDs.

`RoutingService` discovers access and egress for selected transit modes, then
calls `MultimodalPlanner.prepare()` and evaluates the request-local
`PlanningSession` in one `evaluate()` pass, including direct walking and BikeShare
alongside transit results. Reconstruction records failures per journey and keeps
other usable candidates.
The planner owns static corridor discovery, request snapshots, and Phase 2. Its
topology and transfer indexes stay private. A Phase-2 `EvaluationCorridor` has
explicit Origin, BoardingPoint, and Destination anchors. Each edge contains
exactly one `PlannedMovement`: transit, retained walking metrics, or unresolved
walking. Identical access/egress duplicates resolve to
one movement; conflicting same-stop entries or inconsistent access times are
rejected as invalid queries.
`MultimodalPlanner::prepare()` validates access/egress once before Phase 1 and
corridor conversion. Phase 1 owns the structural transit-leg bound; Phase 2
retains its separate check on realized transfer counts.
Phase 2 evaluates each corridor independently and cannot add anchors or
combine steps from different corridors. `RealizedJourney` is the sole realized
route value: it owns ordered anchors and movements, validates path continuity
and timing, and supplies canonical movement identity for deduplication.
`JourneyReconstructor` owns place lookups, remaining street geometry hydration,
BikeShare station lookup, and candidate assembly; it depends on the street
router, transit runtime, and BikeShare network rather than on `RoutingService`.
Reconstruction relies on immutable `RealizedJourney` movement continuity and
BikeShare anchor consistency, while checking its current stop and hydrated timing.

## Static corridor discovery

`TransitNetwork` stores Via Routes, ordered Patterns, Trips, Service Calendars,
Frequencies, and directional Transfers. `TransitTopologyIndex` stores
`PatternPosition(pattern_id, stop_index)` occurrences by boarding point, so
loop routes remain unambiguous. The static search uses one global K across all
selected transit modes. It ranks structurally distinct corridors using access
and egress walking, Via-preprocessed transfer walking, exact static segment
times where available, derived Bus segment costs, and documented optimistic
geometry estimates otherwise. Bike availability never influences Phase 1.

Sparse Bus stop times do not become fabricated `ScheduledTrip` values. When
terminal runtime is sufficient, preprocessing derives per-segment Bus costs
from stop or shape distances; otherwise it uses geometric estimates. These
costs rank Phase 1 candidates and are not timetable data.

Retained corridors contain equivalent transit legs, timing positions, and
selected Bus stops. Equivalent legs share mode and boarding/alighting points;
Phase 2 can choose any matching Pattern and departure while preserving the
retained structural steps.

`TransitRuntime::network()` is the explicit raw immutable network access used
by static search, timing providers, and data inspection. Health reads revisions
and validates through focused `TransitRuntime` operations. Data builds return
one `BuildOutput`; `BuildDiagnostics` groups GTFS, transfer, and BikeShare
diagnostics.

## Request-local evaluation

Walking is always implicit and is not a `selected_modes` value. An empty mode
list skips transit discovery and Phase 1, then evaluates a direct walking
Origin-to-Destination corridor. Selecting only BikeShare also skips
transit search and timing requests while loading one BikeShare availability
snapshot. Before realtime loading, `BikeShareMovementPlanner::required_station_ids`
collects all stations linked to evaluation boarding anchors and all stations within
1,500 metres of geographic endpoints, including direct corridor endpoints.
Availability filtering precedes the existing three-station candidate limits.
The TDX provider caches individual station records for 45 seconds, rechecks
original observation freshness, and fetches only missing/stale IDs in filtered
50-station batches grouped by city (up to four cities concurrently). Partial
failures preserve successful records; empty requests make no availability calls.
The direct corridor is evaluated first, outside static Top-K, followed by every
prepared transit corridor in deterministic order. Phase 2 has no expansion-count
termination condition. Deadlines remain hard failures even after a completed
corridor; the existing frontier, duration, transfer, and provider limits remain.

For each edge, `FixedCorridorEvaluator` explicitly expands Transit, Walking,
then BikeShare in `evaluator/expansion.rs`. Transit resolves retained equivalent
patterns and multiple snapshot departures. Walking reuses retained access,
transfer, and egress metrics without another P2 street request. Unresolved
Walking requests a directional 1×1 Matrix at the state's DepartAt time,
normalized to Taipei. `PlanningSession` owns the `WalkingCache`, including its
street router and request-local metrics cache, and an optional concrete
`BikeShareSession`. The session retains station discovery, availability filtering,
Matrix batching, and result caching. Candidate discovery caches eligible station
choices by anchor pair and requires at least one distinct pickup/dropoff pair;
it does not estimate movement duration. `CorridorAnchor` and its exact cache key
live in `routing/corridor.rs`. Rental transitions and `ActiveBikeRental` live in
`evaluator/state.rs`.
`ExpansionInputs` borrows immutable network, retained corridors, timing, and query.
`ExpansionResolver` executes each state sequentially, keeping cache access, I/O,
deadline checks, and provider failure classification outside pure transitions.
`transit_next_state`, `walking_next_state`, and `bike_next_states` construct states
without evaluator access. Each expansion returns local statistics and failures;
the evaluator aggregates them and prunes once after collecting the edge's successors.
Request-local Walking and BikeShare caches still require mutable access. Phase B
must split cache lookup/update from independent provider work before bounded
parallel expansion can preserve deterministic cache and result ordering.
Walking caches successful, unreachable, and failed Matrix requests by ordered
anchor keys and departure instant across the entire evaluation pass.
`walking_matrix_requests` counts physical P2 Walking Matrix calls, including
failures; `walking_cache_hits` counts reuse. BikeShare's existing realization,
cache-hit, and Matrix counters retain their meanings.

Walking and BikeShare realize actual Matrix metrics before checking query time
limits. Movement expansion checks feasibility without competitiveness screening.
After all modes contribute successors, shared Pareto pruning compares metrics and
compatible state capabilities, subject to the candidate limit.
A candidate containing Walking or BikeShare cannot dominate a distinct candidate
before reconstruction. State dominance likewise requires identical movement
prefixes when either prefix contains street movement; pure transit states retain
metric and capability pruning. This preserves alternatives with independent
hydration failures or different downstream departures. Stable identity still
removes duplicates, and final ranking applies the existing Pareto objectives.
Per-edge frontiers keep the candidate cap;
the service reconstructs the entire bounded Pareto pool before deduplication
and ranking. Ranking applies the final limit, the smaller of its normal result
limit and configured `max_candidates`. Failed reconstruction or ranking rejection
cannot consume an earlier insertion-order slot. The pool is bounded by retained
corridors times the per-edge frontier cap (defaults: 101 × 16) and the request
deadline. `plan_with_stats()` deliberately remains a capped P2
diagnostic API: it returns arrival-ordered journeys without reconstruction or
ranking and reports the capped returned count.
This can increase exact street work compared with baseline-first screening;
request-local caches, deadlines, and station/route limits still bound it.
All P2 movements contain timing and metrics without geometry. Reconstruction
routes every Walking movement (Access, Transfer, Egress, and Direct) and hydrates
BikeShare's pickup walk, bicycle ride, and return walk. Adjacent transfer and
egress movements remain separate and follow their anchors. No synthetic geometry
fallback turns an unreachable street route into a usable journey. Route durations
and distances determine final legs and ranking metrics; downstream Transit
boarding is revalidated without a tolerance. Reconstructed arrivals must respect
latest-arrival and maximum-duration limits. BikeShare reuse waits are recomputed
from actual return times plus 15 minutes and current reconstructed arrival.
BikeShare considers up to three pickup
and three return stations per endpoint. A continuous rental can extend from
its original anchor to a later corridor anchor, so intermediate anchors need
no station. Separate rentals obey the 15-minute reuse rule. BikeShare requires
a fresh, serviceable pickup with a bike and a return station with a dock; it
includes station access, one bicycle route, and station exit walking. Exact
bicycle legs are capped at 60 minutes and 20 kilometres. Availability is
observed at request time; future DepartAt and ArriveBy inventory is not
forecast.

Transit departures after a movement are selected from the same timing
snapshot using the new arrival time. Retained frontiers use the query's candidate
limit; P2 state expansion has no count budget. Walking and
BikeShare cannot add transit stops, patterns, or connections.

## Static and live data

`via-data` reads the supplied local GTFS archive and does not download or
refresh it. GTFS is the only static transit source. TDX IDs are normalized to
existing Via IDs when their formats map directly; unmapped IDs are reported.
The server has no database or identity-reconciliation layer.

GTFS does not contain a YouBike station catalog. To build the static station
metadata, `via-data build` can take local TDX city-scoped
`Bike/Station/City` JSON snapshots. It maps their station IDs to the same
city-scoped Via IDs used by availability, then precomputes bounded directional
walking links with Valhalla. Live bike and dock counts, service state, and
freshness come from TDX request-time availability. Static station scopes are
configured once in `tdx::BIKE_CITIES` using the official Bike API scopes. Without station snapshots,
the generated BikeShare artifact remains empty.

Keep the dependency direction `domain ← routing ← app/api`. Providers
implement domain-facing interfaces. iOS remains feature-oriented
(`App`, `Components`, `Domain`, `Features`, `Resources`, `Services`), with
production planning through `JourneyPlanner → HTTPJourneyPlanner → APIClient`.
The server parses runtime environment settings at its executable entrypoints
and passes typed configuration into provider and data-fetch clients. Production
requires HTTPS for configured upstream URLs; `via-data build` keeps its
separate 15-second Valhalla request timeout.

## Essential commands

```bash
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path server/Cargo.toml --all-targets --all-features
```

See [`docs/dev-tooling.md`](docs/dev-tooling.md) for data and route-debug
commands.

Runtime and preprocessing configuration is centralized in `server/config/via.toml`; see `server/docs/configuration.md` for loading, override precedence, and artifact rebuild requirements.
