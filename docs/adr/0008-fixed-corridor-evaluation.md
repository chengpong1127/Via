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
   one availability snapshot. A synchronous evaluator processes each retained
   corridor as an ordered anchor DAG. Transit edges may use equivalent Patterns
   and any feasible departure from the snapshot; the departure is selected
   using the state's current ready time. Walking edges use the retained surface
   movement cost. BikeShare can add a bounded forward shortcut between corridor
   anchors, including across one or more transit and walking edges.

Phase 2 does not search transit topology, add transit anchors, or combine
portions of different corridors. A BikeShare shortcut may skip anchors already
present in its retained corridor. Each shortcut represents one continuous
rental: only its first and last anchors require a serviceable BikeShare station,
so intermediate anchors need neither docks nor another pickup. After a shortcut,
downstream departures are queried from the same TimingSnapshot using its new
arrival time. Direct walking also uses the journey path representation and may
have a direct BikeShare alternative.

The evaluator keeps a bounded Pareto frontier within each retained corridor.
BikeShare edges consider at most eight forward anchors and three pickup and
three return stations per endpoint; bicycle legs are capped at 60 minutes and
20 kilometres. The request's expansion and candidate limits bound evaluation.
Equivalent transit legs share mode and boarding/alighting points, so Pattern
substitution does not change the retained transit structure.

Bus realtime is fetched by unique stop, grouped by TDX city scope, and filtered
locally. The static `TransferShortcutIndex` keeps directional incoming and
outgoing maps. BikeShare availability is request-local: pickup requires a
fresh serviceable station with a bike, and return requires a fresh serviceable
station with a dock. Feasible pairs are evaluated with bicycle matrix requests
chunked to the street router's source and target limits. A direct walking
journey considers up to three nearby available pickup and return stations at
each endpoint and uses walking and bicycle matrix requests before evaluation.

## Consequences

The Phase 2 evaluator has no provider I/O and has no alternate strategy
dispatcher. Timing, BikeShare availability, and street matrix realizations are
prepared before synchronous evaluation. Request snapshots are a possible
future cache seam; this decision adds no cross-request TTL or distributed
cache.

GTFS contains no BikeShare station catalog. `via-data build` can take local TDX
Taipei and New Taipei Bike Station City JSON snapshots to create static
station metadata and bounded directional walking links. Without those inputs,
the BikeShare artifact is empty. TDX remains the source of request-time station
availability.

The former routing-algorithm trait, Trip-Based implementation, departure
resolver, and asynchronous transfer resolver are not part of the architecture.
