<!-- CODEGRAPH_START -->
## CodeGraph

When `.codegraph/` exists, use `codegraph_explore` or `codegraph explore` before
grep, find, or reading files to locate or understand code. Skip it if the index is
absent.
<!-- CODEGRAPH_END -->

## Engineering constraints

Do not add comments to code in `src` directories.

Consult [CONTEXT.md](CONTEXT.md) for the current architecture. Keep the dependency
direction `domain ← routing ← app/api`, with providers implementing domain-facing
interfaces. Do not reintroduce `planner`, `normalization`, or `routing/engine`
modules, a generic routing-algorithm boundary, Trip-Based routing, async transfer
resolvers, or a `JourneyPlanningService` wrapper.

`MultimodalPlanner` owns static round-based (RAPTOR-style) transit corridor
discovery and fixed-corridor evaluation. Each round adds one ride; keep the best
corridor for each ride count plus near-best corridors up to `max_static_corridors`,
weighting transfers with the ranking transfer buffers, without an expansion cap.
Include the direct walking corridor outside that set and evaluate it first. Skip transit search when no transit mode is selected;
`selected_modes` contains only optional non-walking modes. All corridors use one
evaluation pass. Expand Transit, Walking, then BikeShare and collect feasible
successors before shared Pareto pruning. Check surface feasibility using actual
Matrix metrics, without optimistic-duration screening. Fixed-corridor evaluation
cannot add anchors, combine corridors, or make TDX calls. Load timing only for
retained transit positions and BikeShare availability only when selected. Keep
request-local snapshots and routing caches, and allow a continuous BikeShare
rental across later anchors. `RealizedJourney` owns movement continuity, timing,
and identity; reconstruction consumes it directly. Do not add a RouteGraph to
the canonical path. Static transfer shortcut lookups are directional; Bus realtime
is fetched by unique stop and scope and filtered locally.

GTFS is the only static transit source. Do not fabricate scheduled Bus trips from
sparse stop times. Derived segment costs are not timetable data. BikeShare station
metadata requires local TDX snapshots; availability remains request-time data.
Preserve direct GTFS-to-TDX identifier mapping and report unmapped IDs; do not add
a database or identity-reconciliation layer.

Keep iOS feature-oriented. Production planning flows through `JourneyPlanner →
HTTPJourneyPlanner → APIClient`; retain `DemoJourneyPlanner` for previews and offline
tests. `Features/Navigation` is not part of the current implementation.
