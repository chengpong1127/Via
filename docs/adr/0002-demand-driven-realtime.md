---
Status: Current
---

# Demand-driven realtime resolution

## Context

Via keeps static transit and schedule data in one immutable, versioned
`TransitNetwork`. `TripBasedRouter` asks a provider-neutral departure resolver
only for the departures required by its access/egress frontier. Service
conditions remain on the realtime status interface. An in-process async cache
coalesces identical status misses for a short configurable TTL, while normalized
timestamps and freshness remain authoritative for eligibility. Realtime values
are request-local after collection and are not persisted as a global snapshot.
Transit modes do not share one global realtime-first policy: each mode may have
its own realtime implementation, static representation, fallback behavior, and
timing reliability policy.

## Decision

Use demand-driven, request-scoped resolution through a `DepartureDispatcher`.
The dispatcher only selects a `TransitDepartureSource` by the service's
`TransitMode`, groups batches, and restores query order. The selected source
decides whether fresh usable realtime can answer each query and falls back to
its own static source when necessary. A source may have no realtime
implementation; THSR is intentionally static-only.

Keep the status cache as a performance optimization and leave correctness in
the normalized status interface and freshness evaluation. Bus departure timing
is implemented directly in `BusDepartureSource`, which calls the shared
transport-only `TdxClient`; it does not depend on `ServiceConditionProvider`, an
arrival query, or a generic status cache. `TimingBasis` records
provenance (`RealtimePrediction`, `DelayAdjustedSchedule`, `Schedule`, or
`FrequencyEstimate`). `TimingReliability` is a separate source-owned policy;
static timing is not inherently unreliable. TRA departure timing is also
implemented directly in `TraDepartureSource`: its exact `tra:{TrainNo}`
identity is matched to TDX TRA `TrainLiveBoard` records, and a fresh provider
delay adjusts the existing scheduled trip shape. Missing, stale, malformed,
or failed TRA realtime returns the source's static schedule result.

## Consequences

Planning avoids a global feed refresh architecture and does not require a
global Live Status store. Realtime failures degrade only the affected mode
queries to that mode's static policy, preserving mixed batches where some
queries use realtime and others use fallback. Bus batching remains bounded and
deduplicated inside the bus source. Upstream failures can still temporarily
fail a query when its static source also fails. `CachedServiceConditionProvider`
remains only for service-condition status collection, and owns
`ServiceConditionMetrics`; `BusDepartureSource` owns `BusTimingMetrics`.

Typed domain IDs are derived directly from TDX identifiers as described in
ADR-0006.
