# Canonical RouteGraph

Status: Current

## Context

Walking, transit, and BikeShare candidates need one shared representation for
bounded Pareto search. Earlier planner-era names made it unclear whether a
candidate network, transit backbone, or multimodal graph was authoritative.

## Decision

`RouteGraph` is Via's canonical routing representation. It contains graph
nodes and `Walk`, `Transit`, and `Bike` edges. Transfer connectors are walking
edges, and direct walking is a normal graph edge when the street provider
confirms it is reachable.

## Consequences

Search and reconstruction share one public routing mental model. BikeShare is
expanded at the walking-edge seam, and no compatibility graph types or
parallel planner abstractions are needed.
