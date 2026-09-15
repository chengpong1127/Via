---
Status: Current
---

# Demand-driven realtime resolution

## Context

Via keeps static transit and schedule data in one immutable, versioned
`TransitNetwork`. `TripBasedRouter` asks a provider-neutral departure resolver
only for the departures required by its access/egress frontier. Service
conditions and bike-share stations remain on the realtime status interface. An
in-process async cache
coalesces identical misses for a short configurable TTL, while normalized
timestamps and freshness remain authoritative for eligibility. Realtime values
are request-local after collection and are not persisted as a global snapshot.

## Decision

Use demand-driven, request-scoped realtime resolution from the configured
departure and status interfaces. Keep the cache as a performance optimization
and leave correctness in the normalized provider interface and freshness
evaluation.

## Consequences

Planning avoids a global feed refresh architecture and does not require a
global Live Status store. Upstream failures can degrade or temporarily fail
the request according to the existing eligibility and failure rules.

This supersedes the realtime acquisition, refresh, and snapshot-storage parts
of ADR-0001. Typed domain IDs are now derived directly from TDX identifiers as
described in ADR-0006.
