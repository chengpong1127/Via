---
Status: Current
---

# Preserve TDX-derived typed identifiers where GTFS maps directly

## Context

Via's server is stateless. GTFS is authoritative for static transit data and
TDX supplies realtime data. The server needs GTFS static records and TDX
realtime records to join when both describe the same entity; it does not
currently combine multiple static providers or maintain an identity registry.

## Decision

Keep Via-owned opaque domain ID types. Normalize GTFS stop and service IDs to
existing TDX-derived values when the GTFS identifier encodes the same authority
and entity key. TDX realtime normalization keeps using the pure TDX ID
constructors. Do not add a provider-alias registry, persistent identity map,
or database-backed reconciliation layer. The GTFS build reports IDs that do
not map to current TDX realtime IDs.

Bus service IDs retain their matching GTFS/TDX route UID when available. Metro
service IDs retain Via's current line and direction composite. Directional
patterns are separate services; where the current TDX feed has no corresponding
GTFS ID, the builder keeps a deterministic GTFS-based ID and reports it as
unmapped.

## Consequences

Routing retains type safety without maintaining a second Via identity
universe. Static-network revisions remain stateless. An upstream ID change
creates a new typed ID in the next artifact; realtime joins continue wherever
the GTFS and TDX identifiers have a direct canonical mapping.
