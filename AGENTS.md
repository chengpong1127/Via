<!-- CODEGRAPH_START -->
## CodeGraph

When `.codegraph/` exists, use `codegraph_explore` or `codegraph explore` before
grep, find, or reading files to locate or understand code. Skip it if the index is
absent.
<!-- CODEGRAPH_END -->

## Engineering constraints

Do not add comments to code in `src` directories.

Consult [CONTEXT.md](CONTEXT.md) for the current architecture and
[docs/adr/](docs/adr/) for decisions that constrain it. Keep the dependency
direction `domain ← routing ← app/api`, with providers implementing domain-facing
interfaces. Do not reintroduce `planner`, `normalization`, or `routing/engine`
modules, a generic routing-algorithm boundary, Trip-Based routing, async transfer
resolvers, or a `JourneyPlanningService` wrapper.

Walking is always available; `selected_modes` contains only optional non-walking
modes. Fixed-corridor evaluation cannot add anchors or combine corridors, and
reconstruction consumes `RealizedJourney` directly. Keep request-local snapshots
and caches and avoid additional TDX calls during evaluation.

GTFS is the only static transit source. Do not fabricate scheduled Bus trips from
sparse stop times. Derived segment costs are not timetable data. BikeShare station
metadata requires local TDX snapshots; availability remains request-time data.
Preserve direct GTFS-to-TDX identifier mapping and report unmapped IDs; do not add
a database or identity-reconciliation layer.

Keep iOS feature-oriented and retain `DemoJourneyPlanner` for previews and offline
tests. `Features/Navigation` is not part of the current implementation.
