# Via current architecture

Via plans multimodal journeys using a GTFS-built static transit network, TDX
realtime timing and BikeShare availability, and Valhalla street routing.

API and CLI requests convert into domain-owned `PlanningInput` before entering
routing. The canonical server flow is `RoutingService → access discovery →
corridor search → corridor evaluation → RealizedJourney → JourneyReconstructor →
ranking`, coordinated by `MultimodalPlanner`. Routing depends on domain
interfaces, including `BikeAvailabilitySource`; providers supply their
implementations.

Routing modules follow the pipeline:

| Module | Step |
| --- | --- |
| `access` | `AccessDiscovery` finds walkable boarding points near the origin and destination |
| `corridors` | Corridor search (Phase 1): static round-based transit corridors, with the topology and transfer indexes it uses |
| `planner` | `MultimodalPlanner` runs corridor search, loads request-local snapshots and runs corridor evaluation |
| `evaluation` | Corridor evaluation (Phase 2): `CorridorEvaluator`, evaluation corridors and BikeShare movement |
| `journey` | `RealizedJourney`, the timed result of evaluation |
| `reconstruct` | Street geometry, timing recheck and ranking |
| `cache` | Request-local Matrix cache and the reconstruction Route cache |

Corridor search runs a static round-based (RAPTOR-style) search in which each
round adds one ride; it keeps the best corridor for each ride count, then
the cheapest corridor for each set of transit modes, so optimistic derived Bus
costs cannot crowd out Metro or Rail, plus near-best corridors up to the
configured cap, with transfers weighted by the
ranking buffers. Corridor evaluation uses request-local snapshots to evaluate
fixed corridors plus the direct walking corridor, which starts first and may
overlap transit evaluation within the configured concurrency limit. Walking is
always enabled; selected modes are optional transit and BikeShare modes. With no
transit mode, corridor search is skipped.

`MultimodalPlanner::plan` runs corridor search, builds evaluation corridors,
loads request-local snapshots and hands them to `CorridorEvaluator`, which
returns journeys, statistics and observed provider failures or a single routing
error. For each fixed corridor it expands Transit, Walking, then BikeShare along
fixed anchors and prunes feasible successors. It cannot add anchors, combine
corridors, or make TDX calls. States and transit corridors can run concurrently
within configured limits; outcomes retain deterministic ordering. Walking and
cycling Matrix and reconstruction Route requests are time-independent, so
conditional road restrictions can differ from a time-specific route.
Reconstruction hydrates street geometry, rechecks timing and ranks usable
journeys. `RealizedJourney` owns movement continuity, timing and its identity
signature. `JourneyReconstructor::reconstruct_all` bounds reconstruction
concurrency, keeps candidate order and collects reconstruction failures.

Routing implementation modules are private; supported entry types are exported
at the routing module root. Corridor evaluation reaches BikeShare through the
`BikeMovementSource` interface; the production `BikeShareSession` combines
candidate selection and Matrix realization behind one request-local cache. The
shared `cache` module holds the request-local Matrix cache used by walking and
BikeShare, and the Route cache used by reconstruction with its own retry policy.

`via-data build` consumes local GTFS and optional TDX station snapshots; it does
not download them. `via-data fetch` can create source snapshots. Sparse Bus stop
times never become fabricated scheduled trips; derived segment costs support
static search only. GTFS IDs map to TDX-derived Via IDs where possible, with
unmapped IDs reported. There is no identity database.

Production iOS planning uses `JourneyPlanner → HTTPJourneyPlanner → APIClient`;
`DemoJourneyPlanner` supports previews and offline tests.

iOS navigation runs on-device against the selected journey. Walking and cycling
guidance carries `geometry_index` (into the leg geometry) and `street_name`
parameters; transit legs carry `intermediate_stops` for the stop countdown.
`NavigationTracker` projects fixes forward along each leg, counts down stops,
raises action-only alerts and flags off-route or missed connections;
`NavigationModel` re-plans from the current position with a 60 s cooldown to
respect the TDX rate limit.

See [server/README.md](server/README.md) for local development and verification,
[configuration](server/docs/configuration.md) for runtime settings, and the
[VIA project](https://app.notion.com/p/408aefae2f748154b5ebcdf042f8c303)
for architecture decisions, operations and testing guidance.
