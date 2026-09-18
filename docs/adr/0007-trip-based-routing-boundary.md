---
Status: Current
---

# Routing algorithm boundary and Trip-Based search

## Context

Via needs runtime transit timing and multimodal transfers without exposing
Trip-Based queue internals to the service or provider adapters. Static
TDX-derived transit identities and demand-driven realtime remain authoritative.

## Decision

`RoutingAlgorithm` is the boundary between `RoutingService` and the current
`TripBasedRouter`. The router owns `TripBasedIndex` and its trip-segment search
state.

The router consumes the shared static `TransitNetwork`, a `DepartureResolver`,
and a `TransferIndex`. A resolver receives `(service, boarding point, ready
time)` and returns complete request-scoped `EffectiveTrip` instances. Runtime
trip IDs are synthetic unless a scheduled trip identity is available; they are
not persistent aliases.

`TransferShortcutIndex` is a direct index of static public-transit interchange
edges from `TransitNetwork.transfer_edges`. A `TransferShortcut` contains only
boarding-point endpoints and a static walking/interchange duration. Static
transit↔BikeShare walking connectors are persisted in the same
`TransitNetwork` artifact and indexed separately by the BikeShare operator.
The query-time operator owns realtime station eligibility and batched bicycle
movement. TripBased collects BikeShare expansion needs while scanning alights,
expands them once per round, and represents successful interchanges as explicit
`JourneySegment::BikeShareTransfer` values.

## Consequences

Trip-Based departure resolution is lazy and request-local, while cached provider
calls can still be coalesced per frontier. Schedule, realtime, delay-adjusted,
and frequency policies remain provider composition details. There is no
separate static transfer artifact: the canonical network persists transit
transfer edges and transit↔BikeShare connectors, and runtime derives their
separate in-memory indexes from that network. Transfer reconstruction has no
street-router or matrix dependency; surviving BikeShare legs are hydrated only
during final reconstruction.

Reconstruction consumes `JourneyCandidate` directly; `RouteGraph` is not part
of the production architecture.
