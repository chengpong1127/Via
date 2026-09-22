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

`RoutingService` owns one concrete `TransitPlanner` with two planning phases:

1. **Destination Reachability** searches backward over service patterns and
   static directional transfers. It uses selected modes and a transit-leg
   budget, but no time information.
2. **Temporal Multimodal Planning** performs all request-local timing work
   before search. Bus realtime is fetched by unique stop, grouped by TDX city
   scope, and chunked inside `BusDepartureSource`. Other modes retain
   provider-native batching. When selected, BikeShare availability is loaded
   once concurrently with transit timing. Temporal search then runs forward
   and synchronously over local snapshots and may replace an adjacent transit
   movement or static walking transfer with a faster BikeShare segment. It
   never adds a new transit anchor.

The compact topology index stores `(service_id, stop_index)` occurrences so
loop routes are unambiguous. The static transfer index keeps both incoming and
outgoing maps. Temporal dominance retains the earliest ready time for each
`(boarding point, legs used)` pair.

Direct walking remains independent and walking-only. BikeShare does not replace
access or egress, and only substitutes an existing adjacent movement between
boarding-point anchors after temporal candidate generation.

## Consequences

TDX access is a bounded preprocessing step and is unreachable from temporal
search. A Bus rate limit stops the remaining chunks for that scope. Structural
reachability cannot reject a route because its schedule has ended or realtime
is unavailable. The request-local snapshot is a clean future cache seam, but
this decision adds no cross-request TTL or distributed cache.

The former routing-algorithm trait, Trip-Based implementation, departure
resolver, and async transfer resolver are no longer part of the architecture.
