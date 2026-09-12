---
status: accepted
---

# Demand-driven realtime enrichment

Via keeps static transit and schedule data in one immutable, versioned
`TransitNetwork`. Planning first generates scheduled candidates, then asks a
provider-neutral realtime interface only for the arrivals, service conditions,
and bike-share stations those candidates require. An in-process async cache
coalesces identical misses for a short configurable TTL, while normalized
timestamps and freshness remain authoritative for eligibility. Realtime values
are request-local after collection and are not persisted as a global snapshot.

This supersedes the realtime acquisition, refresh, and snapshot-storage parts
of ADR-0001. ADR-0001's provider-neutral typed identities and versioned alias
registry remain in force.
