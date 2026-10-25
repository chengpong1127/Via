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
   BoardingPoint, and Destination anchors. Bounded access and egress choices
   remain as baseline alternatives at their edges. With no transit mode
   selected, Phase 1 is skipped and a trivial Origin-to-Destination corridor
   has no baseline movement. If retained transit corridors produce no
   candidate, this same trivial corridor is evaluated as fallback.

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
other modes available; budget exhaustion and deadlines abort evaluation.

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
`PlanningSession` carries caches and statistics across primary and fallback
evaluation.

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
target limits. A surface-only fallback corridor uses the same Walking and
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

GTFS contains no BikeShare station catalog. `via-data build` can take local TDX
configured Taiwan Bike Station City JSON snapshots to create static
station metadata and bounded directional walking links. Without those inputs,
the BikeShare artifact is empty. TDX remains the source of request-time station
availability.

Runtime wiring uses a concrete `DepartureResolver` for mode-owned timing
sources. The former routing-algorithm trait, Trip-Based implementation, and
asynchronous transfer resolver are not part of the architecture.
