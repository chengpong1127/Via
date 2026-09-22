---
Status: Current
---

# Reachability, timing snapshot, and temporal routing

## Context

Incremental realtime lookups during transit search multiplied service-stop
needs into many TDX requests and continued requesting after rate limits. Static
timetable feasibility could also hide structurally valid paths that a delayed
realtime vehicle would make usable.

## Decision

`RoutingService` owns one concrete `TransitPlanner` with three transit phases:

1. `DestinationReachability` searches backward over service patterns and static
   directional transfers. It uses selected modes and a transit-leg budget, but
   no time information.
2. `TimingSnapshotLoader` performs all request-local timing work before search.
   Bus realtime is fetched by unique stop, grouped by TDX city scope, and
   chunked inside `BusDepartureSource`. Other modes retain provider-native
   batching. Realtime failure uses the source's static fallback policy.
3. Temporal search runs forward and synchronously over local network and
   snapshot data. A boarding must be present in destination reachability and fit
   the remaining-leg budget before timing is read.

The compact topology index stores `(service_id, stop_index)` occurrences so
loop routes are unambiguous. The static transfer index keeps both incoming and
outgoing maps. Temporal dominance retains the earliest ready time for each
`(boarding point, legs used)` pair.

Direct walking remains independent. BikeShare remains an augmentation of
direct-walking or public-transit skeletons after transit candidate generation.

## Consequences

TDX access is a bounded preprocessing step and is unreachable from temporal
search. A Bus rate limit stops the remaining chunks for that scope. Structural
reachability cannot reject a route because its schedule has ended or realtime
is unavailable. The request-local snapshot is a clean future cache seam, but
this decision adds no cross-request TTL or distributed cache.

The former routing-algorithm trait, Trip-Based implementation, departure
resolver, and async transfer resolver are no longer part of the architecture.
