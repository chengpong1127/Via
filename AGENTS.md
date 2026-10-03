## Agent skills

### Source code comments

Do not add comments to code in `src` directories.

### Current architecture

The server's canonical planning flow is:

```text
RoutingService
  → MultimodalPlanner.prepare → PlanningSession.evaluate_primary / evaluate_fallback
  → static transit corridors → EvaluationCorridor anchors
  → request-local timing / availability snapshots
  → FixedCorridorEvaluator (baseline + Walking / BikeShare surface movement)
  → RealizedJourney → reconstruct → rank
```

`MultimodalPlanner` owns both phases. Phase 1 runs one global, bounded forward
Top-K search over selected transit modes using static schedule costs, derived
Bus segment costs, or optimistic geometry estimates. It retains transit
corridors only. Phase 2 represents each retained corridor as an
`EvaluationCorridor` whose anchors include Origin, boarding points, and
Destination; bounded access and egress alternatives remain attached to their
edges. With no transit mode selected, Phase 1 is skipped and the planner builds
a surface-only Origin-to-Destination corridor. The session evaluates primary
corridors with `evaluate_primary()` and can evaluate its one-shot fallback with
`evaluate_fallback()` if evaluation or reconstruction yields no candidate.
Fallback state stays private to the session.

`selected_modes` lists only optional non-walking modes. `[]` enables walking
alone; `[bike_share]` enables walking and BikeShare without transit search.
The planner loads timing only for retained transit positions and loads one
request-local BikeShare availability snapshot only when BikeShare is selected.
`FixedCorridorEvaluator` evaluates each baseline and uses
`SurfaceMovementPlanner` implementations for Walking and BikeShare. Both use
cheap optimistic bounds before exact street routing and request-local caches.
BikeShare may extend one continuous rental across later anchors. The evaluator
cannot add anchors or combine corridors and makes no additional TDX calls.
Static `TransferShortcutIndex` lookups are directional; Bus realtime is fetched
by unique stop and scope, then filtered locally. `RealizedJourney` owns the
ordered anchors and movements, validates their continuity and timing, and
supplies canonical movement identity for deduplication. Reconstruction consumes
that journey directly. There is no `RouteGraph` in the canonical routing path.
Keep the dependency direction `domain ← routing ← app/api`, with providers
implementing domain-facing interfaces. Do not reintroduce the
removed `planner`, `normalization`, or `routing/engine` modules, a generic
routing-algorithm boundary, Trip-Based routing, async transfer resolvers, or a
`JourneyPlanningService` wrapper.

iOS remains feature-oriented (`App`, `Components`, `Domain`, `Features`,
`Resources`, `Services`). Production planning flows through
`JourneyPlanner → HTTPJourneyPlanner → APIClient`; keep
`DemoJourneyPlanner` for previews and offline tests. Navigation under
`Features/Navigation` is not part of the current implementation.

GTFS is the only static transit source. `via-data` reads the supplied local
archive; it does not download or refresh it. TDX APIs are used for realtime
timing and BikeShare availability. Local TDX Bike Station
City snapshots can provide static station metadata to `via-data build`; live
counts remain request-time availability. Normalize GTFS IDs to existing
TDX-derived Via IDs when their formats map directly, and report IDs that do
not map. The server has no database or identity-reconciliation layer.

Sparse Bus stop times do not become fabricated `ScheduledTrip` values. Derive
separate per-segment Bus costs for Phase 1 from terminal runtime and stop/shape
distances, using geometric Phase 1 costs when the runtime is insufficient.
These derived costs are not timetable data. GTFS does not supply a BikeShare
station catalog, so the build's BikeShare artifact is empty unless local TDX
station snapshots are provided.

The retained ADRs in `docs/adr/` record only non-obvious current constraints.
`CONTEXT.md` is the quick current-state reference; completed issue research and
spec archives are not part of the active repository documentation.
