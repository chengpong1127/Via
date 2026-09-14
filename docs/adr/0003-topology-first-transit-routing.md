# Topology-first transit routing

Status: Current

## Context

Transit discovery must remain useful when a representative scheduled trip is
not active today, while departure timing still needs current schedule or live
arrival evidence. Treating a timetable row as the topology would make the
network disappear before realtime enrichment can make the decision.

## Decision

Discover destination-oriented transit topology first. Enrich only the bounded
services and boarding points that survive discovery with schedule and
demand-driven realtime evidence. Keep the final multimodal search and
eligibility decisions in Via's routing pipeline.

## Timing boundary

Transit timing is resolved after topology discovery through one
provider-neutral `TransitTimingProvider` contract. Bus, Metro, conventional
rail, and high-speed rail all become normalized departures before they enter
the shared `TimedTransitRide` and `RouteGraph` models. Timing adapters own live
prediction, schedule, delay, or frequency-estimation policy; routing does not
branch on `TransitMode` to choose a source.

## Consequences

Topology remains stable across service-day and feed-availability differences,
while timing and reliability stay explicit. Discovery, enrichment, and search
can be bounded independently without introducing a second transit graph.
