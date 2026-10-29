# Fixed corridor evaluation

Status: Current

## Context

Realtime calls during transit search multiplied provider requests, while a second
topology search after corridor discovery made the retained structure unpredictable.

## Decision

`MultimodalPlanner` discovers a bounded global Top-K of static transit corridors,
then loads request-local timing and selected BikeShare availability. It evaluates
those corridors and a direct walking corridor in one pass. Walking is always
available; optional modes determine transit search and BikeShare availability.

Each `EvaluationCorridor` has fixed ordered anchors and one planned movement per
edge. The evaluator can substitute equivalent transit patterns and expand Walking
and BikeShare alternatives, but cannot add anchors or combine corridors. Surface
feasibility uses actual Matrix metrics. A continuous BikeShare rental can extend
across later anchors. `RealizedJourney` owns and validates the resulting movements;
reconstruction hydrates them and rechecks timing before ranking.

## Consequences

Phase 2 makes no additional TDX calls or transit topology searches. Frontier,
provider and deadline limits bound evaluation. Distinct unhydrated street paths
are retained conservatively because hydration can change feasibility and timing.
Walking and cycling street requests are time-independent, which can differ from
routes subject to time-dependent restrictions. Request-local caches share work;
no cross-request routing cache is introduced. See [CONTEXT.md](../../CONTEXT.md)
for the current execution flow and concurrency.
