# Via current architecture

Via plans multimodal journeys using a GTFS-built static transit network, TDX
realtime timing and BikeShare availability, and Valhalla street routing.

`RoutingService → MultimodalPlanner.prepare → PlanningSession.evaluate →
RealizedJourney → reconstruction → ranking` is the canonical server flow.
Phase 1 finds a bounded global Top-K of static transit corridors. Phase 2
loads request-local snapshots and evaluates fixed corridors plus the direct
walking corridor, which runs first. Walking is always enabled; selected modes
are optional transit and BikeShare modes. With no transit mode, Phase 1 is skipped.

`FixedCorridorEvaluator` expands Transit, Walking, then BikeShare along fixed
anchors and prunes feasible successors. It cannot add anchors, combine corridors,
or make TDX calls. States and transit corridors can run concurrently within
configured limits; outcomes retain deterministic ordering. Walking and cycling
Matrix and reconstruction Route requests are time-independent, so conditional
road restrictions can differ from a time-specific route. Reconstruction hydrates
street geometry, rechecks timing and ranks usable journeys. `RealizedJourney`
owns movement continuity, timing and identity.

`via-data build` consumes local GTFS and optional TDX station snapshots; it does
not download them. `via-data fetch` can create source snapshots. Sparse Bus stop
times never become fabricated scheduled trips; derived segment costs support
static search only. GTFS IDs map to TDX-derived Via IDs where possible, with
unmapped IDs reported. There is no identity database.

Production iOS planning uses `JourneyPlanner → HTTPJourneyPlanner → APIClient`;
`DemoJourneyPlanner` supports previews and offline tests.

See [server/README.md](server/README.md) for local development and verification,
[configuration](server/docs/configuration.md) for runtime settings, and the
[VIA project](https://app.notion.com/p/408aefae2f748154b5ebcdf042f8c303)
for architecture decisions, operations and testing guidance.
