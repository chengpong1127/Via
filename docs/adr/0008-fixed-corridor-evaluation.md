# Fixed corridor evaluation

Status: Current

## Context

Incremental realtime calls during transit search multiplied stop needs into
many TDX requests and could continue after a rate limit. A separate topology
search during Phase 2 also made the request's structural result depend on a
second search after Phase 1 had already retained candidate corridors.

## Decision

`TransitPlanner` owns the two phases:

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

`FixedCorridorEvaluator` processes each corridor's ordered edges. It evaluates
baseline successors, then checks surface alternatives using
`SurfaceMovementPlanner` implementations for Walking and BikeShare. Each
planner uses an optimistic duration lower bound before requesting an exact
street route; realized endpoint routes are cached within the request. Transit
edges may use equivalent Patterns and feasible departures from the timing
snapshot, selected using the state's current ready time. Walking surface
movements are carried into the realized path, so reconstruction does not
repeat their street route. The evaluator never searches transit topology, adds
anchors, or combines portions of different corridors.

BikeShare checks nearby usable stations and exact routes only when its lower
bound can compete with the baseline. An active rental can extend from its
original start anchor to the next structural anchor, so intermediate anchors
need neither docks nor another pickup. Exact Bike routes are cached within the
request. Separate rentals wait 15 minutes after the bike is returned; the
return time is the pickup walk plus bicycle ride, before the final walk to the
anchor. Downstream departures use the same timing snapshot and each
successor's current ready time.

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
station with a dock. A competitive movement's station pairs are evaluated in
one or more bicycle matrix requests chunked to the street router's source and
target limits. A trivial fallback corridor uses the same Walking and BikeShare
planners and constraints as any other Phase-2 edge.

## Consequences

Phase 2 performs no additional transit topology search or TDX calls. It uses
one request-local timing snapshot for retained positions and, when selected,
one BikeShare availability snapshot. It may make lazy Valhalla route or matrix
calls after surface lower-bound screening. Exact surface routes are cached
only within the request; this decision adds no cross-request TTL or distributed
cache.

GTFS contains no BikeShare station catalog. `via-data build` can take local TDX
Taipei and New Taipei Bike Station City JSON snapshots to create static
station metadata and bounded directional walking links. Without those inputs,
the BikeShare artifact is empty. TDX remains the source of request-time station
availability.

Runtime wiring uses a concrete `DepartureResolver` for mode-owned timing
sources. The former routing-algorithm trait, Trip-Based implementation, and
asynchronous transfer resolver are not part of the architecture.
