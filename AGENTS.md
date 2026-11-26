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
interfaces. Do not reintroduce a top-level `planner` module, `normalization`, or
`routing/engine` modules, a generic routing-algorithm boundary, Trip-Based routing,
async transfer resolvers, or a `JourneyPlanningService` wrapper. Keep routing
modules organized by pipeline step (`access`, `corridors`, `planner`, `evaluation`,
`journey`, `reconstruct`); `routing/planner` only coordinates the steps and loads
request snapshots.

`MultimodalPlanner` owns static round-based (RAPTOR-style) transit corridor
discovery and fixed-corridor evaluation. Each round adds one ride; keep the best
corridor for each ride count, then the cheapest corridor for each set of transit
modes, plus near-best corridors up to `max_static_corridors`,
weighting transfers with the ranking transfer buffers, without an expansion cap.
Include the direct walking corridor outside that set and evaluate it first. Skip transit search when no transit mode is selected;
`selected_modes` contains only optional non-walking modes. All corridors use one
evaluation pass. Expand Transit, Walking, then BikeShare and collect feasible
successors before shared Pareto pruning. Check surface feasibility using actual
Matrix or Route metrics, without optimistic-duration screening. Fixed-corridor evaluation
cannot add anchors, combine corridors, or make TDX calls. Load timing only for
retained transit positions and BikeShare availability only when selected. Keep
request-local snapshots and routing caches, and allow a continuous BikeShare
rental across later anchors. `RealizedJourney` owns movement continuity, timing,
and identity; reconstruction consumes it directly. Do not add a RouteGraph to
the canonical path. Static transfer shortcut lookups are directional; Bus realtime
is fetched by unique stop and scope and filtered locally.

GTFS is the only static transit source. TDX Bus Route snapshots supply the
parent RouteUID of each Bus sub-route for realtime matching, and TDX Bus Schedule
snapshots supply headways only for the estimates below. Do not fabricate
scheduled Bus trips from sparse stop times. Derived segment costs are not timetable data;
`preprocessing.bus_derived_segment_scale_percent` rescales the Bus segment times that are
not timetable data (`Derived`, `Estimated`) at build time, calibrated from TDX vehicle
progress, and never touches `Schedule` segments.
The one allowed departure estimate: for a Bus pattern with no GTFS trips or
frequencies, after the last fresh TDX prediction at a stop, add departures one
headway apart (by service period) for at most two hours. They use
`FrequencyEstimate` timing with `Fallback` reliability, are never scheduled trips,
and the leg carries `estimated: true` in the API and a visible label in iOS. A real
prediction or scheduled trip always wins; never add estimates without a fresh
prediction anchor. GTFS inexact frequencies (`FrequencyEstimate`) carry
`estimated: true` too. BikeShare station
metadata requires local TDX snapshots; availability remains request-time data.
Preserve direct GTFS-to-TDX identifier mapping and report unmapped IDs; do not add
a database or identity-reconciliation layer.

Keep iOS feature-oriented. Production planning flows through `JourneyPlanner →
HTTPJourneyPlanner → APIClient`, and the app always plans for departing now.
`Features/Navigation` follows a selected `JourneyOption` on-device:
`NavigationTracker` snaps location fixes to leg geometry and transit stops, and
`NavigationModel` re-routes through `JourneyPlanner` with at most one plan per
minute. There is no separate navigation backend.

Keep `ios/Via/Resources/Localizable.xcstrings` in step with the code. Any change
that adds, renames, or removes a user-facing string updates the catalog in the same
commit: add the key with its zh-Hant translation and delete keys the code no longer
uses. Format-only keys such as `%@ %@` and the app name need no translation.
