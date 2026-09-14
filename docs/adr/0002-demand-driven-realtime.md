---
Status: Current
---

# Demand-driven realtime enrichment

## Context

Via keeps static transit and schedule data in one immutable, versioned
`TransitNetwork`. Planning first generates topology candidates, then asks a
provider-neutral transit timing interface only for the departures those
candidates require. Service conditions and bike-share stations remain on the
realtime status interface. An in-process async cache
coalesces identical misses for a short configurable TTL, while normalized
timestamps and freshness remain authoritative for eligibility. Realtime values
are request-local after collection and are not persisted as a global snapshot.

## Decision

Use demand-driven, request-scoped realtime enrichment after bounded candidate
discovery. Keep the cache as a performance optimization and leave correctness
in the normalized provider interface and freshness evaluation.

## Consequences

Planning avoids a global feed refresh architecture and does not require a
global Live Status store. Upstream failures can degrade or temporarily fail
the request according to the existing eligibility and failure rules.

This supersedes the realtime acquisition, refresh, and snapshot-storage parts
of ADR-0001. Typed domain IDs are now derived directly from TDX identifiers as
described in ADR-0006.
