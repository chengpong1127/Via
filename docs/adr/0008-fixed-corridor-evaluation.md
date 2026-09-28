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
   applies across modes.
2. **Time-dependent multimodal realization** loads one request-local timing
   snapshot for the retained corridor positions and, when BikeShare is selected,
   one availability snapshot. A forward evaluator processes each retained
   corridor's ordered anchors. Transit edges may use equivalent Patterns and
   any feasible departure from the snapshot; the departure is selected using
   the state's current ready time. Walking edges use the retained surface
   movement cost. For each next anchor, BikeShare first checks a cheap optimistic
   lower bound from nearby usable stations. Valhalla routing is requested only
   when that bound can beat the best normal arrival from the same state.

Phase 2 does not search transit topology, add transit anchors, or combine
portions of different corridors. Normal Transit and Walk successors are
evaluated first for each structural edge. A BikeShare state can either finalize
its rental and continue by Transit or Walk, or lazily extend the same rental
from its original start anchor to the next structural anchor. An extension
replaces the prior Bike shortcut, so intermediate anchors need neither docks
nor another pickup. Exact Bike routes are cached by physical stop pair for the
request. Separate rentals wait 15 minutes after the bike is returned; the
return time is the pickup walk plus bicycle ride, before the final walk to the
anchor. Downstream departures are queried from the same TimingSnapshot using
each successor's current ready time. Direct walking also uses the journey path
representation and may have a direct BikeShare alternative.

The evaluator keeps a bounded Pareto frontier within each retained corridor.
BikeShare considers up to three pickup and three return stations per endpoint;
its lower bound uses straight-line distance and an intentionally optimistic
bicycle speed. Exact bicycle legs are capped at 60 minutes and 20 kilometres.
Only the current next anchor is considered, except that an active continuous
Bike run can be extended from its original start to that next anchor. The
request's expansion and candidate limits bound evaluation. Equivalent transit
legs share mode and boarding/alighting points, so Pattern substitution does not
change the retained transit structure.

Bus realtime is fetched by unique stop, grouped by TDX city scope, and filtered
locally. The static `TransferShortcutIndex` keeps directional incoming and
outgoing maps. BikeShare availability is request-local: pickup requires a
fresh serviceable station with a bike, and return requires a fresh serviceable
station with a dock. A competitive movement's station pairs are evaluated in
one or more bicycle matrix requests chunked to the street router's source and
target limits. A direct walking journey considers up to three nearby available
pickup and return stations at each endpoint and uses walking and bicycle matrix
requests before evaluation.

## Consequences

Phase 2 performs no additional transit topology search or TDX calls. It uses
one request-local TimingSnapshot and one BikeShare availability snapshot, and
may make lazy Valhalla matrix calls after the optimistic Bike lower-bound gate.
Exact Bike routing is cached only within the request; this decision adds no
cross-request TTL or distributed cache.

GTFS contains no BikeShare station catalog. `via-data build` can take local TDX
Taipei and New Taipei Bike Station City JSON snapshots to create static
station metadata and bounded directional walking links. Without those inputs,
the BikeShare artifact is empty. TDX remains the source of request-time station
availability.

The former routing-algorithm trait, Trip-Based implementation, departure
resolver, and asynchronous transfer resolver are not part of the architecture.
