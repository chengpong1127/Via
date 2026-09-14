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

## Consequences

Topology remains stable across service-day and feed-availability differences,
while timing and reliability stay explicit. Discovery, enrichment, and search
can be bounded independently without introducing a second transit graph.
