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
trip IDs are synthetic unless a scheduled run identity is available; they are
not persistent aliases.

`TransferShortcutIndex` stores multiple walking/YouBike transfer alternatives
and preserves decomposed path metadata for reconstruction. YouBike availability
is a separate runtime constraint and never implements `DepartureResolver`. The
current shortcut builder is ULTRA-compatible in representation; exact ULTRA
preprocessing may replace the builder when the offline street artifact
supports it.

## Consequences

Trip-Based departure resolution is algorithmically lazy and request-local,
while cached provider calls can still be coalesced per frontier. Schedule,
realtime, delay-adjusted, and frequency policies remain provider composition
details. RAPTOR or CSA may later implement `RoutingAlgorithm` with unrelated
internal indexes. Reconstruction consumes `JourneyCandidate` directly;
`RouteGraph` is not part of the production architecture.
