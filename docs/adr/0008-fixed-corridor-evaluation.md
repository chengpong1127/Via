# Fixed corridor evaluation

Status: Current

## Context

Incremental realtime calls during transit search multiplied stop needs into
many TDX requests and could continue after a rate limit. A separate topology
search during Phase 2 also made the request's structural result depend on a
second search after Phase 1 had already retained candidate corridors.

## Decision

`MultimodalPlanner` owns the two phases:

1. **Static corridor discovery** performs one bounded forward Top-K search
   across selected transit modes. It ranks structural corridors using exact
   static segment costs, derived Bus costs, or optimistic geometry. A single K
   applies across modes. It returns transit structure only.
2. **Phase-2 corridor construction and realization** converts each retained
   transit corridor to an `EvaluationCorridor` with explicit Origin,
   BoardingPoint, and Destination anchors. Each edge contains exactly one
   `PlannedMovement`: transit, retained walking metrics, or unresolved walking.
   A direct Origin-to-Destination corridor with one unresolved walking edge is
   included outside Top-K and evaluated before transit corridors. With no transit mode
   selected, Phase 1 is skipped. All corridors use one evaluation pass.

`selected_modes` contains optional non-walking modes. Walking is always
available. An empty list skips transit search and timing calls; `[bike_share]`
also skips them and requests one availability snapshot. The planner loads
timing for retained transit positions and availability only when BikeShare is
selected. An empty timing query set does not call the timing provider.

`FixedCorridorEvaluator` processes each corridor's ordered edges as a unified
movement expansion search. `ExpansionInputs` borrows immutable inputs and
`ExpansionResolver` resolves Transit, Walking, then BikeShare sequentially in
`evaluator/expansion.rs`. Pure transition functions construct successors from
resolved movements without touching caches, providers, statistics, or deadlines.
Each state returns local work counts and failures for evaluator aggregation.
A concrete `WalkingCache` owns request-local Matrix results; BikeShare borrows
an optional concrete request-local session. The evaluator collects all feasible
successors before shared per-edge Pareto pruning and candidate finalization.

Transit expands equivalent Patterns and multiple feasible departures from the
timing snapshot using each state's ready time and transfer constraints. Walking
owns retained access, transfer, and egress metrics and realizes unresolved Walking
with directional 1×1 Matrix requests. Retained metrics suppress duplicate P2 street
requests. Matrix constraints preserve DepartAt time normalized to Taipei.
`WalkingCache` retains successes, unreachable cells, and failures by ordered
anchor keys and departure instant throughout the evaluation pass. BikeShare keeps
availability filtering, station discovery, batching, and exact result caching in
`BikeShareSession` and `BikeShareMovementPlanner`. Candidate discovery is cached by
anchor pair; eligible station choices require at least one distinct pickup/dropoff
pair and are reused for realization without a duration estimate. Provider failures
are classified centrally and leave other modes available. Deadlines abort evaluation. Every prepared corridor is evaluated unless a real
error or deadline prevents completion.

Walking and BikeShare perform actual Matrix realization before checking hard
query time limits. Movement implementations determine physical and temporal
feasibility without heuristic competitiveness screening. Later arrival alone
cannot discard a route with less walking, fewer transfers, less cycling, fewer
rentals, or different future capabilities. Feasible successors enter the shared
multi-objective frontier; per-edge and final Pareto pruning remain bounded.

Walking surface movements are carried into the `RealizedJourney`, the sole
realized path value. It owns ordered anchors and metric-only movements, validates
path continuity and timing, and supplies canonical movement identity for
deduplication. Reconstruction generates Walking and BikeShare geometry with
Route calls. Direct Walking is hydrated like Access, Transfer, and Egress.
Transfer and Egress follow their separate anchors; the former terminal shortcut
and synthetic unreachable fallback are removed. Route timing determines final
legs and ranking metrics, and downstream Transit departures are revalidated.
Late arrivals, missed departures, and maximum-duration violations reject that
candidate without suppressing other candidates. BikeShare reuse waits derive from
actual return time plus 15 minutes and current reconstructed arrival. The evaluator never searches
transit topology, adds anchors, or combines portions of different corridors.

An active BikeShare rental can extend from its original start anchor to the next
structural anchor, so intermediate anchors need neither docks nor another pickup.
The extension replaces the earlier movement using the existing rental transition.
Separate rentals wait 15 minutes after the bike is returned; the return time is
the pickup walk plus bicycle ride, before the final walk to the anchor. Downstream
departures use the same timing snapshot and each successor's current ready time.
`PlanningSession` owns request-local caches, snapshots, and statistics for the
single evaluation pass.

The evaluator keeps a bounded Pareto frontier within each retained corridor.
BikeShare considers up to three pickup and three return stations per endpoint;
availability and distinct-station pairing are checked before realization. Exact
bicycle legs are capped at 60 minutes and 20 kilometres.
Only the current next anchor is considered, except that an active continuous
Bike run can be extended from its original start to that next anchor. The
request's frontier and duration limits bound evaluation. Equivalent transit
legs share mode and boarding/alighting points, so Pattern substitution does
not change the retained transit structure.

Bus realtime is fetched by unique stop, grouped by TDX city scope, and filtered
locally. The static `TransferShortcutIndex` keeps directional incoming and
outgoing maps. BikeShare availability is request-local: pickup requires a
fresh serviceable station with a bike, and return requires a fresh serviceable
station with a dock. A feasible movement's station pairs are evaluated in
one or more bicycle matrix requests chunked to the street router's source and
target limits. The direct walking corridor uses the same Walking expansion and
BikeShare session and constraints as any other Phase-2 edge.

## Consequences

Phase 2 performs no additional transit topology search or TDX calls. It uses
one request-local timing snapshot for retained positions and, when selected,
one BikeShare availability snapshot. It may make lazy Valhalla Matrix calls
for eligible movements without optimistic-duration screening. P2 stores no street
geometry. Walking's
`walking_matrix_requests` counts physical Matrix calls, including failures;
`walking_cache_hits` counts reused results. Reconstruction Route calls are
separate work and use actual returned metrics. A 1×1 Matrix is not assumed faster
than Route. Caches remain request-local; no cross-request cache is introduced.

`CorridorAnchor` and `CorridorAnchorKey` live in `routing/corridor.rs`.
`PlanningSession` explicitly owns `WalkingCache` and an optional `BikeShareSession`.
Rental transitions and `ActiveBikeRental` live in `evaluator/state.rs`. A test-only
BikeShare injection seam preserves synthetic rental and expansion regressions; production
uses no generic BikeShare dispatch.

The direct corridor runs first, followed by transit corridors in static relative
order. No Phase-2 expansion counter terminates evaluation. Work counters are
observational; deadline expiry remains a hard error after any completed corridor.

Phase A remains sequential. Request-local Walking and BikeShare caches require
mutable access in the resolver. Phase B must separate their lookup/update boundary
from provider execution, return per-expansion cache/statistics effects, bound
concurrent provider work, and merge outcomes in input order before pruning. No
synchronization or concurrent state execution is introduced here.

A candidate containing any street movement cannot dominate a distinct candidate
before geometry and timing are confirmed. State dominance requires identical
movement prefixes when either contains street movement, in addition to the usual
metric and capability comparisons. Pure transit states retain ordinary dominance.
This conservative policy also preserves later transit departures when Route takes
longer than Matrix. Stable identity deduplication and final Pareto ranking remain.
Per-edge candidate caps still apply. The service
reconstructs the bounded Pareto pool, deduplicates stable keys, and ranks all usable
candidates before final selection. The final result cap is the smaller of the
ranking policy limit and configured `max_candidates`. The reconstruction pool is
bounded by corridor count times the frontier cap (101 × 16 with defaults) and
the deadline. This may hydrate more candidates than the
old first-N reconstruction rule; it avoids losing later preferred or eligible
journeys. `plan_with_stats()` remains an arrival-ordered, capped P2 diagnostic API
without reconstruction or ranking; its candidate count is the returned count.

Identical access/egress duplicates resolve to one movement. Conflicting entries
for a stop, departures inconsistent with query time, or access availability
inconsistent with walking duration produce `InvalidQuery` before Phase 1.

The direct-corridor regression checks one additional Walking request and one
state expansion without changing transit lookups. Deterministic budget-removal
measurements and the Phase B boundary are recorded in
`docs/performance/phase-a-expansion.md`.

GTFS contains no BikeShare station catalog. `via-data build` can take local TDX
configured Taiwan Bike Station City JSON snapshots to create static
station metadata and bounded directional walking links. Without those inputs,
the BikeShare artifact is empty. TDX remains the source of request-time station
availability.

Runtime wiring uses a concrete `DepartureResolver` for mode-owned timing
sources. The former routing-algorithm trait, Trip-Based implementation, and
asynchronous transfer resolver are not part of the architecture.
