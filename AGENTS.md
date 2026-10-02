## Agent skills

### Current architecture

The server's canonical planning flow is:

```text
street access/egress → TransitPlanner
  → static Top-K structural corridors → request-local snapshots
  → forward fixed corridor evaluator (Transit / Walk / lazy YouBike routing)
  → reconstruct → rank
```

`TransitPlanner` is the single concrete transit planner. Phase 1 performs one
global, bounded forward Top-K search across the selected transit modes using
static schedule durations or optimistic geometry estimates. Phase 2 loads one
request-local timing snapshot for retained corridors and, when selected, one
BikeShare availability snapshot. An asynchronous forward fixed-corridor
evaluator chooses equivalent transit Patterns and departures, then evaluates
Walk and normal successors before screening BikeShare with a cheap optimistic
lower bound. Competitive BikeShare movements are routed lazily through the
street provider, and a continuous rental may extend from its original anchor
to the next corridor anchor. The evaluator cannot add anchors or combine
corridors and makes no additional TDX calls. A direct origin-to-destination
BikeShare alternative remains separate. The
static `TransferShortcutIndex` exposes directional `transfers_from` and
`transfers_to` lookups. Bus realtime is fetched by unique stop and scope, then
filtered locally. BikeShare does not add transit connectivity, replace access
or egress, or create direct BikeShare-only routes. Reconstruction consumes the
planner's final path variants and there is no `RouteGraph` in the canonical
routing path. Keep the dependency
direction `domain ← routing ← app/api`, with providers implementing domain-facing
interfaces. Do not reintroduce the removed `planner`, `normalization`, or
`routing/engine` modules, a generic routing-algorithm boundary, Trip-Based
routing, async transfer resolvers, or a `JourneyPlanningService` wrapper.

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
