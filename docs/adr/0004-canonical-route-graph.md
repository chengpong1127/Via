# Canonical RouteGraph

Status: Superseded by ADR-0007

## Context

Walking, transit, and BikeShare candidates need one shared representation for
bounded Pareto search. Earlier planner-era names made it unclear whether a
candidate network, transit backbone, or multimodal graph was authoritative.

## Decision

This decision is retired. `RouteGraph` was removed rather than retained as a
compatibility seam. `RoutingAlgorithm` returns `JourneyCandidate` values and
reconstruction consumes their access, transit, transfer, and egress data
directly.

## Consequences

The canonical routing flow is now street access/egress, `RoutingAlgorithm`,
`TripBasedRouter`, direct reconstruction, and ranking. BikeShare is represented
by decomposed transfer paths; generic graph search is not part of production
routing.
