# BikeShare as walking-edge substitution

Status: Superseded by ADR-0007

## Context

BikeShare can replace a walking connection before transit, after transit, or
between selected points. Modeling only special transit gaps would miss direct
and post-transit opportunities and would duplicate connector logic.

## Decision

Represent BikeShare as a decomposed transfer shortcut containing walking access,
a bicycle leg between rental stations, and walking egress. Station availability
is fetched only for stations referenced by explored candidate shortcuts and is
evaluated for the request before a shortcut is used.

This remains the representation decision, but the old walking-edge graph
expansion implementation is retired. Direct BikeShare requests are reconstructed
from the same decomposed path shape without introducing a transit service.

## Consequences

BikeShare participates in Trip-Based candidate search when it connects transit
stops. Zero inventory or return capacity removes the shortcut, while station
evidence and safety thresholds remain visible during reconstruction and ranking.
