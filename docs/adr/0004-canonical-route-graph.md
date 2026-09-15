# Canonical RouteGraph

Status: Superseded by ADR-0007

## Context

Walking, transit, and BikeShare candidates need one shared representation for
bounded Pareto search. Earlier planner-era names made it unclear whether a
candidate network, transit backbone, or multimodal graph was authoritative.

## Decision

`RouteGraph` was Via's canonical routing representation for the initial
multimodal implementation. It remains a compatibility seam for detailed
reconstruction during the migration, but algorithms must not be required to
convert their internal state into it.

## Consequences

Search and reconstruction share one public routing mental model. BikeShare is
expanded at the walking-edge seam, and no compatibility graph types or
parallel planner abstractions are needed.
