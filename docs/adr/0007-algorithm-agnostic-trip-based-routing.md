# Algorithm-agnostic Trip-Based routing

Status: Current

## Context

Via needs runtime transit timing and multimodal transfers without binding all
future routing algorithms to Trip-Based queues or one provider's
identity model. Static TDX-derived transit identities and demand-driven
realtime remain authoritative.

## Decision

`RoutingAlgorithm` is the small public boundary between `RoutingService` and a
routing implementation. The first implementation is `TripBasedRouter`, which
owns `TripBasedIndex` and its trip-segment search state.

Algorithms consume the shared static `TransitNetwork`, a `DepartureResolver`,
and a `TransferIndex`. A resolver receives `(service, boarding point, ready
time)` and returns complete request-scoped `EffectiveTrip` instances. Runtime
trip IDs are synthetic unless a scheduled trip identity is available; they are
not persistent aliases.

`TransferShortcutIndex` is a direct index of static public-transit interchange
edges from `TransitNetwork.transfer_edges`. A `TransferShortcut` contains only
boarding-point endpoints and a static walking/interchange duration. BikeShare
remains a demand-driven endpoint connector before or after transit; its paths
and availability are owned by endpoint routing and never appear in
`JourneySegment::Transfer`.

## Consequences

Trip-Based departure resolution is algorithmically lazy and request-local,
while cached provider calls can still be coalesced per frontier. Schedule,
realtime, delay-adjusted, and frequency policies remain provider composition
details. There is no separate static transfer artifact: the canonical network
persists the transfer edges and runtime derives its in-memory index from that
network. Transfer reconstruction has no street-router or matrix dependency.
RAPTOR or CSA may later implement `RoutingAlgorithm` with unrelated internal
indexes.
Reconstruction consumes `JourneyCandidate` directly;
`RouteGraph` is not part of the production architecture.
