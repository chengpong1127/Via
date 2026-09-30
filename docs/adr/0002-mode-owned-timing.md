# Mode-owned timing snapshots

Status: Current

## Context

Timing sources have different provider formats, batching limits, and fallback
rules. Loading timing during topology expansion can multiply stop needs into
many provider calls and tie transit search to provider behavior.

## Decision

Phase 1 discovers static Top-K corridors. Before fixed corridor evaluation,
`TimingSnapshotLoader` loads the timing positions for their retained transit
legs. The dispatcher delegates to the mode-owned timing providers, which keep
their parsing and static fallback rules. Bus requests are deduplicated by
unique stop and TDX scope, chunked at the provider boundary, then filtered
locally against the requested Via positions. Other modes retain their native
batching. THSR remains static-only.

## Consequences

The forward fixed-corridor evaluator is asynchronous, consumes the immutable
snapshot, and makes no additional TDX calls. Transit and walking successors
use snapshot values; competitive BikeShare movements may make lazy Valhalla
calls after the optimistic lower-bound check. A rate limit stops remaining Bus
chunks for the affected scope, and unresolved values use static timing fallback.
`TimingSnapshot` is request-local; this decision adds no shared cache or
distributed cache.
