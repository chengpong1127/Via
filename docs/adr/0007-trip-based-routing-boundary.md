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
and a batch `TransferResolver`. A departure resolver receives `(service, boarding point, ready
time)` and returns complete request-scoped `EffectiveTrip` instances. Runtime
trip IDs are synthetic unless a scheduled trip identity is available; they are
not persistent aliases.

`DepartureResolver` is implemented by a `DepartureDispatcher`. It groups
queries by `TransitMode` and delegates to `BusDepartureSource`,
`MetroDepartureSource`, `TraDepartureSource`, or `ThsrDepartureSource`. The
dispatcher contains no mode-specific timing policy. Each source owns its static
and optional realtime timing, batching strategy, realtime/static fallback, and
`TimingReliability` policy. Bus departure timing calls the shared, thin
`TdxClient` directly; its endpoint paths, TDX DTOs, freshness, and fallback
remain in the `timing/bus*.rs` source modules. The service-condition provider
is not in this departure path. `TimingBasis` remains provenance only,
so a static THSR
timetable can be `Reliable` while bus static fallback or Metro frequency
timing can be `Fallback`.

TripBased handles same-stop interchange itself. For movement between distinct
BoardingPoints, it collects transfer needs during a round and resolves them in
one batch. The current `TransferResolver` implementation reads a
`TransferShortcutIndex` derived from `TransitNetwork.transfer_edges`. Each
resolved transfer carries its arrival time, walking cost, and reconstruction
segment. Future dynamic movement can sit behind this boundary without changing
TripBased search. Static transfer edges remain in the `TransitNetwork` artifact.

## Consequences

Trip-Based departure resolution is lazy and request-local, while cached provider
calls can still be coalesced per frontier. Schedule, realtime, delay-adjusted,
and frequency policies remain provider composition details. There is no
separate static transfer artifact: the canonical network persists transit
transfer edges and runtime derives their in-memory index from that network.
Transfer reconstruction has no street-router or matrix dependency.

Reconstruction consumes `JourneyCandidate` directly; `RouteGraph` is not part
of the production architecture.
