## Agent skills

### Current architecture

The server's canonical planning flow is:

```text
street access/egress → TransitPlanner
  → DestinationReachability → TimingSnapshot → temporal search
  → optional BikeAvailabilitySnapshot → multimodal variants
  → reconstruct → rank
```

`TransitPlanner` is the single concrete transit planner. Phase 1 performs a
backward topology-only search. Phase 2 loads one request-local timing snapshot
and, when selected, one BikeShare availability snapshot before synchronous
forward search and per-segment BikeShare alternatives. The static
`TransferShortcutIndex` exposes directional `transfers_from` and `transfers_to`
lookups. Bus realtime is fetched by unique stop and scope, then filtered
locally. BikeShare only replaces an existing adjacent boarding-point movement;
it does not add transit connectivity, access/egress alternatives, or direct
BikeShare-only routes. Reconstruction consumes the planner's final path
variants and there is no `RouteGraph` in the canonical routing path. Keep the
dependency
direction `domain ← routing ← app/api`, with providers implementing domain-facing
interfaces. Do not reintroduce the removed `planner`, `normalization`, or
`routing/engine` modules, a generic routing-algorithm boundary, Trip-Based
routing, async transfer resolvers, or a `JourneyPlanningService` wrapper.

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
