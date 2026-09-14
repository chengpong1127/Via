---
Status: Current
---

# Use TDX-derived typed identifiers

## Context

Via's server is stateless and TDX is the authoritative source for Taiwan
transit static and realtime data, including expected nationwide expansion. The
server needs static and realtime records for the same TDX entity to join
reliably; it does not currently combine multiple providers describing the same
logical entity.

## Decision

Keep Via-owned opaque domain ID types, but derive their underlying values
directly from the relevant TDX identifiers. Static artifact generation and TDX
realtime normalization use the same pure constructors. TDX UIDs already carry
the authority code needed by the current feeds, so Via does not add a
provider-alias registry, persistent identity mapping, or database-backed
reconciliation layer.

The current service artifact may identify a service by `SubRouteUID`. When an
artifact models each direction separately, it uses a deterministic composite
key (`SubRouteUID` plus direction); realtime normalization and query projection
use the same key semantics.

## Consequences

Routing retains type safety without maintaining a second Via identity universe.
Static-network revisions remain stateless: an upstream ID change creates the
new typed ID in the new artifact, provided that artifact and its realtime feed
use the same TDX identity. Provider-neutral reconciliation can be introduced
later if a real multi-provider requirement appears.
