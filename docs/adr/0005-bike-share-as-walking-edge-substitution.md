# BikeShare as walking-edge substitution

Status: Current

## Context

BikeShare can replace a walking connection before transit, after transit, or
between selected points. Modeling only special transit gaps would miss direct
and post-transit opportunities and would duplicate connector logic.

## Decision

Expand BikeShare opportunities from the discovered walking base graph. A
usable opportunity adds walking access, a bicycle edge between rental
stations, and walking egress; station availability is fetched and evaluated
for the request before the edges are added.

## Consequences

BikeShare participates in the same Pareto search as walking and transit. Zero
inventory or return capacity removes the substituted edge, while station
evidence and safety thresholds remain visible during reconstruction and
ranking.
