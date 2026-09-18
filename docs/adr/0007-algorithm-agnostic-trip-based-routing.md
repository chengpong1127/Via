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
boarding-point endpoints and a static walking/interchange duration. Static
transit↔BikeShare walking connectors are persisted in the same
`TransitNetwork` artifact and indexed separately by `BikeShareConnectorIndex`.
The query-time `BikeShareOperator` owns realtime station eligibility and
batched bicycle movement. TripBased collects BikeShare expansion needs while
scanning alights, expands them once per round, and represents successful
interchanges as explicit `JourneySegment::BikeShareTransfer` values.

## Consequences

Trip-Based departure resolution is algorithmically lazy and request-local,
while cached provider calls can still be coalesced per frontier. Schedule,
realtime, delay-adjusted, and frequency policies remain provider composition
details. There is no separate static transfer artifact: the canonical network
persists the transit transfer edges and transit↔BikeShare connectors, and runtime
derives their separate in-memory indexes from that network. Transfer
reconstruction has no street-router or matrix dependency; surviving BikeShare
legs are hydrated only during final reconstruction.
RAPTOR or CSA may later implement `RoutingAlgorithm` with unrelated internal
indexes.
Reconstruction consumes `JourneyCandidate` directly;
`RouteGraph` is not part of the production architecture.
