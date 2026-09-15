# Topology-first transit routing

Status: Superseded by ADR-0007

## Historical context

Transit discovery must remain useful when a representative scheduled trip is
not active today, while departure timing still needs current schedule or live
arrival evidence. Treating a timetable row as the topology would make the
network disappear before realtime enrichment can make the decision.

## Superseded decision

Discover destination-oriented transit topology first. Enrich only the bounded
services and boarding points that survive discovery with schedule and
demand-driven realtime evidence. Keep the final multimodal search and
eligibility decisions in Via's routing pipeline.

## Timing boundary

The current architecture does not use this topology-first pipeline. Departure
selection occurs lazily inside `TripBasedRouter` through `DepartureResolver`,
after algorithm-neutral access/egress preparation; see ADR-0007.

## Consequences

The historical separation is retained here for context only. It is not a
production dependency and must not be reintroduced as a second transit search
pipeline.
