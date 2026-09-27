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
2. **Fixed corridor evaluation** loads one request-local timing snapshot for
   the retained corridor positions, and optionally one BikeShare availability
   snapshot. A synchronous evaluator processes each retained corridor in
   order. Transit steps may use equivalent Patterns and any feasible departure
   from the snapshot. Surface transfers may use their walking realization or a
   feasible YouBike realization.

Phase 2 does not search topology, add anchors, or combine portions of different
corridors. It keeps a bounded Pareto frontier within each retained corridor.
Equivalent transit legs share mode and boarding/alighting points, so Pattern
substitution does not change the retained structure.

Bus realtime is fetched by unique stop, grouped by TDX city scope, and filtered
locally. The static `TransferShortcutIndex` keeps directional incoming and
outgoing maps. BikeShare availability is request-local: pickup requires a
fresh serviceable station with a bike, and return requires a fresh serviceable
station with a dock. Each surface-transfer anchor considers no more than three
pickup and three return stations, and uses one batched bicycle matrix operation
for feasible station pairs.

## Consequences

The Phase 2 evaluator has no provider I/O and has no alternate strategy
dispatcher. Request snapshots are a possible future cache seam; this decision
adds no cross-request TTL or distributed cache.

GTFS contains no BikeShare station catalog. `via-data build` can take local TDX
Taipei and New Taipei Bike Station City JSON snapshots to create static
station metadata and bounded directional walking links. Without those inputs,
the BikeShare artifact is empty. TDX remains the source of request-time station
availability.

The former routing-algorithm trait, Trip-Based implementation, departure
resolver, and asynchronous transfer resolver are not part of the architecture.
