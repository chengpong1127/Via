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
movement expansion search. `TransitExpander`, `WalkingExpander`, and
`BikeShareExpander` implement one `MovementExpander` interface. The expansion
context carries the current edge and anchors, query, request-local surface
planners, and shared accounting. The evaluator dispatches expansion, prunes
states, and finalizes candidates; it does not select a baseline winner.

Transit expands equivalent Patterns and multiple feasible departures from the
timing snapshot using each state's ready time and transfer constraints. Walking
owns retained access, transfer, and egress metrics as well as exact street walking;
retained metrics suppress duplicate exact walking requests. Surface providers
retain their existing caches, availability filtering, station discovery, and
routing responsibilities. Provider failures are classified centrally and leave
other modes available. Deadlines abort evaluation; budget exhaustion retains
only candidates from completed corridors.

Optimistic duration bounds screen surface movements only against query time
limits, not the arrival of another mode. This intentionally replaces the former
baseline-first policy: later arrival alone cannot discard a route with less
walking, fewer transfers, less cycling, fewer rentals, or different future
capabilities. Exact successors enter the shared multi-objective frontier.

Walking surface movements are carried into the `RealizedJourney`, the sole
realized path value. It owns ordered anchors and movements, validates path
continuity and timing, and supplies canonical movement identity for deduplication;
reconstruction does not repeat their street route. The evaluator never searches
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
its lower bound uses straight-line distance and an intentionally optimistic
bicycle speed. Exact bicycle legs are capped at 60 minutes and 20 kilometres.
Only the current next anchor is considered, except that an active continuous
Bike run can be extended from its original start to that next anchor. The
request's expansion and candidate limits bound evaluation. Equivalent transit
legs share mode and boarding/alighting points, so Pattern substitution does
not change the retained transit structure.

Bus realtime is fetched by unique stop, grouped by TDX city scope, and filtered
locally. The static `TransferShortcutIndex` keeps directional incoming and
outgoing maps. BikeShare availability is request-local: pickup requires a
fresh serviceable station with a bike, and return requires a fresh serviceable
station with a dock. A feasible movement's station pairs are evaluated in
one or more bicycle matrix requests chunked to the street router's source and
target limits. The direct walking corridor uses the same Walking and
BikeShare planners and constraints as any other Phase-2 edge.

## Consequences

Phase 2 performs no additional transit topology search or TDX calls. It uses
one request-local timing snapshot for retained positions and, when selected,
one BikeShare availability snapshot. It may make lazy Valhalla route or matrix
calls after surface lower-bound screening. Exact surface routes are cached
only within the request; this decision adds no cross-request TTL or distributed
cache. Removing arrival-only screening increases exact routing work for feasible
trade-offs, including extensions of slower rentals. The 12-edge regression
fixture now performs 72 distinct exact BikeShare realizations and serves 66
additional requests from cache, compared with zero exact requests under the old
baseline screen. Expansion and candidate limits remain mandatory; no untested
heuristic cutoff replaces the removed screen. The bounded frontier can still
truncate Pareto alternatives when its candidate limit is reached.

Walking is a concrete dependency, and BikeShare occupies one optional typed slot
in `SurfacePlanners`. The narrow `BikeSharePlanner` interface supports deterministic
synthetic movement tests. `SurfaceMovementPlanner`, `SurfaceKind`, the mixed
movement enum, collection scans, and integer planner indices are removed.

The direct corridor runs first and naturally charges at most two expansions
(one Walking realization and one optional BikeShare realization). This protects
an early opportunity for both surface modes without a scheduler or increased
limits. Transit corridors retain their static relative order. All calls use the
same hard counter at `charge()`. An exhausted corridor contributes nothing,
including any temporary states on its unfinished edges; previously completed
corridor candidates remain eligible. If none exist, `Phase2EvaluationLimit` is
returned. Deadline expiry remains a hard error even after a completed corridor.
Warnings, an exhaustion counter, and stats report partial evaluation.

A BikeShare-enabled direct corridor may need both expansions to complete;
with only one available, its temporary Walking successor is discarded and
no complete candidate means a limit error.
This policy can leave too little budget for transit under very small limits.
It preserves completed work rather than guaranteeing that every mode can finish
under an insufficient budget. The one-edge direct corridor needs no separate
allowance implementation: two known planner slots bound its charged work.

A journey still requiring street geometry hydration cannot prune an already-routed
candidate before reconstruction. Per-edge candidate caps still apply. The service
reconstructs the bounded Pareto pool, deduplicates stable keys, and ranks all usable
candidates before final selection. The final result cap is the smaller of the
ranking policy limit and configured `max_candidates`. The reconstruction pool is
bounded by corridor count times the frontier cap (101 × 16 with defaults), the
shared expansion budget, and deadline. This may hydrate more candidates than the
old first-N reconstruction rule; it avoids losing later preferred or eligible
journeys. `plan_with_stats()` remains an arrival-ordered, capped P2 diagnostic API
without reconstruction or ranking; its candidate count is the returned count.

Identical access/egress duplicates resolve to one movement. Conflicting entries
for a stop, departures inconsistent with query time, or access availability
inconsistent with walking duration produce `InvalidQuery` before Phase 1.

The direct-corridor regression checks one additional Walking request and one
expansion without changing transit lookups. At the transit-only budget boundary,
completed transit candidates remain available and evaluation is marked partial.
The 12-edge BikeShare fixture retains 72 exact realizations and 66 cache hits.
Representative measurements and commands live in `docs/route-corpus.md`.

GTFS contains no BikeShare station catalog. `via-data build` can take local TDX
configured Taiwan Bike Station City JSON snapshots to create static
station metadata and bounded directional walking links. Without those inputs,
the BikeShare artifact is empty. TDX remains the source of request-time station
availability.

Runtime wiring uses a concrete `DepartureResolver` for mode-owned timing
sources. The former routing-algorithm trait, Trip-Based implementation, and
asynchronous transfer resolver are not part of the architecture.
