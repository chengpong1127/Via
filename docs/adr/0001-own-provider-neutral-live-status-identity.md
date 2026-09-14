---
Status: Superseded
Superseded by: ADR 0006
---

# Own provider-neutral Live Status identity

## Context

TDX and city feeds can change independently of the real transport network.
Provider identifiers and raw status schemas must not leak into routing or native
client contracts.

## Decision

Via assigns stable typed IDs to network entities and keeps government
identifiers in a versioned alias registry. Routing consumes only normalized
Arrival Estimates, Service Conditions, and Bike Availability through provider
interfaces; provider schemas remain inside adapters.

## Consequences

Provider migrations, identifier churn, and raw status semantics do not force
changes through routing and native clients. Realtime acquisition and caching
remain governed by ADR 0002.
