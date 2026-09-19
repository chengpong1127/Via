## Agent skills

### Current architecture

The server's canonical planning flow is:

```text
street access/egress → RoutingAlgorithm → TripBasedRouter
  → reconstruct → rank
```

`RoutingAlgorithm` is the algorithm-neutral routing boundary. `TripBasedRouter`
owns its Trip-Based index and consumes `DepartureResolver` plus
`TransferResolver`; same-stop interchange stays in TripBased, while the resolver
handles movement between distinct BoardingPoints. The current resolver uses
`TransferShortcutIndex` for static walking. Reconstruction consumes
`JourneyCandidate` directly and
there is no `RouteGraph` in the canonical routing path. Keep the dependency
direction `domain ← routing ← app/api`, with providers implementing domain-facing
interfaces. Do not reintroduce the removed `planner`, `normalization`, or
`routing/engine` modules, or a `JourneyPlanningService` wrapper.

iOS remains feature-oriented (`App`, `Components`, `Domain`, `Features`,
`Resources`, `Services`). Production planning flows through
`JourneyPlanner → HTTPJourneyPlanner → APIClient`; keep
`DemoJourneyPlanner` for previews and offline tests. Navigation under
`Features/Navigation` is not part of the current implementation.

TDX is the current authoritative source for Taiwan transit static and realtime
data. Static artifacts and realtime normalization use the same pure TDX-derived
ID constructors; the server has no database or provider identity-reconciliation layer.

The retained ADRs in `docs/adr/` record only non-obvious current constraints.
`CONTEXT.md` is the quick current-state reference; completed issue research and
spec archives are not part of the active repository documentation.
