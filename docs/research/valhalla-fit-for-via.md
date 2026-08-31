# Valhalla fit for Via's server planner

Research date: 2026-08-29  
Scope: Current official Valhalla documentation and source, reconciled with Via's accepted planner and server decisions.

## Recommendation

**Use Valhalla as a separately deployed street-routing service, not as Via's Journey planner.** It is a strong candidate to own OpenStreetMap ingestion, graph-tile construction, coordinate correlation, and Walking/bicycle path and matrix calculations. Via's Rust Axum service should continue to own Journey grammar, McRangeRAPTOR over Bus/Metro, the pinned Network Snapshot, Live Status eligibility, Reliable/Feasible classification, ranking, and diversity.

This is compatible with the accepted [hybrid routing decision](issue-8-routing-algorithms.md): Valhalla can implement the time-dependent street-search box without replacing the timetable core. It is not compatible with a strict “one Vercel Function plus PostgreSQL and no additional runtime service” constraint. Valhalla is a C++ service built around persistent graph-tile files and caches; Via's existing Vercel research records a 250 MB uncompressed Function bundle limit, 500 MB writable `/tmp`, and no correctness dependence on local files or resident process memory ([Via's Axum/Vercel research](issue-10-axum-vercel.md)). If Via adopts Valhalla, run it on an always-on container or VM near the Axum Function and treat it as a bounded, observable dependency. If the team will not add that deployment, do not embed or rebuild Valhalla inside each Function invocation; choose a Rust-native street kernel instead.

The first step should be a time-boxed prototype using a Taipei–New Taipei extract. Accept it only after measuring tile size, cold/warm service latency, matrix limits, route quality at representative Boarding Points and YouBike Stations, and end-to-end p95 including the Function-to-Valhalla network hop.

## Proposed boundary

```text
Via Axum planner + pinned Network Snapshot
    ├── Valhalla street service, pinned graph revision
    │     ├── correlate origin/destination to walk/bicycle edges
    │     ├── Walking access/egress and Transfer matrices
    │     ├── direct Walking routes
    │     ├── bicycle paths between allowed YouBike Stations
    │     └── selected-leg geometry and maneuver facts
    ├── Via McRangeRAPTOR
    │     └── Bus/Metro timetable candidates and parent chains
    └── Via policy
          ├── gate YouBike pickup/return using pinned Bike Availability
          ├── gate transit using Arrival Estimates and Service Conditions
          ├── classify Reliable/Feasible
          └── rank, prune, diversify, and build Journey Options
```

Valhalla's graph revision must be part of Via's static planning-artifact revision. Do not replace tiles underneath active requests. Publish a new instance or tileset, health-check it, then atomically switch new Planning Requests to it; an in-flight request stays on its initially selected revision. This is an architectural inference required to preserve Via's immutable Network Snapshot contract.

## Capability assessment

| Concern | Valhalla evidence | Fit for Via |
| --- | --- | --- |
| Walking and bicycle routing | Valhalla builds an OSM-backed tiled graph and exposes `pedestrian` and `bicycle` costing. Costing is calculated at runtime from edge and transition attributes rather than baked into the tiles; pedestrian options cover access, walk speed, steps, sidewalks, hills, and related attributes ([route costing](https://valhalla.github.io/valhalla/api/route/api-reference/), [dynamic costing](https://valhalla.github.io/valhalla/concepts/costing/dynamic-costing/)). Depart-at requests use a time-dependent forward A* by default, subject to service limits ([route `date_time`](https://valhalla.github.io/valhalla/api/route/api-reference/#other-request-options)). | **Strong.** Use for Walking and bicycle edge eligibility, duration, distance, path geometry, and static Guidance facts. Via still owns policy calibration and should not equate Valhalla's internal cost with Via's generalized Journey cost. |
| One-to-many/many-to-one access | `/sources_to_targets` accepts separate source and target lists, supports `pedestrian`, `bicycle`, and `bikeshare`, and returns time/distance/cost; it can optionally return shapes. Multimodal transit is explicitly unsupported by this matrix API, and some time-dependent matrix orientations are restricted ([matrix API](https://valhalla.github.io/valhalla/api/matrix/)). | **Strong with testing.** Use Walking matrices for origin→Boarding Points, Boarding Points→destination, and bounded Transfers; use bicycle matrices between admitted YouBike Station sets. Request selected routes separately when richer geometry or maneuvers are needed. Validate exact time-dependent matrix behavior for Via's orientations. |
| Point snapping and map matching | Route and locate inputs are correlated to nearby usable graph edges. `/locate` returns correlated points, candidate edges, access, way IDs, and graph IDs ([locate API](https://valhalla.github.io/valhalla/api/locate/)). Meili's `trace_route` and `trace_attributes` instead map-match a sequence such as a noisy GPS trace ([map-matching API](https://valhalla.github.io/valhalla/api/map-matching/)). | **Strong, with the right API.** Normal Planning Requests normally need route/matrix location correlation; use `/locate` for explicit diagnostics or when Via must retain the correlated point. Do not invoke trace map matching for a single selected origin or destination. |
| Geometry and Guidance | Route responses include Legs, encoded path shapes, and maneuver/narrative data; Thor's `TripLegBuilder` enriches path edges with geometry and attribution needed by Guidance ([route overview](https://valhalla.github.io/valhalla/api/turn-by-turn/overview/), [Thor architecture](https://valhalla.github.io/valhalla/thor/)). | **Useful input, not the wire authority.** Reuse geometry and semantic road facts. Via should map them into its provider-neutral Leg/Guidance contract rather than expose Valhalla response types or rendered instructions directly. |
| Transit planning | Valhalla can ingest raw GTFS and offers `multimodal`, but its documentation says that mode currently combines only pedestrian and transit. Thor implements it as forward A* with transit-schedule lookup and mode switching—not McRangeRAPTOR ([route costing](https://valhalla.github.io/valhalla/api/route/api-reference/#costing-models), [Thor algorithms](https://valhalla.github.io/valhalla/thor/), [Docker GTFS build](https://github.com/valhalla/valhalla/blob/master/docker/README.md#build-valhalla-with-transit)). A July 2025 request for GTFS-Realtime routing support was closed by a Valhalla maintainer with “No plans” ([Valhalla issue #5381](https://github.com/valhalla/valhalla/issues/5381)). | **Do not use.** It would replace Via's accepted McRangeRAPTOR core with a different algorithm, lacks Via's live-transit semantics, and would duplicate Via's TDX normalization/identity work. Keep Bus/Metro outside Valhalla. |
| YouBike | Valhalla's beta `bikeshare` costing switches between Walking and bicycle at graph nodes imported from OSM `amenity=bicycle_rental`; its exposed controls are fixed rent/return costs and penalties ([route costing](https://valhalla.github.io/valhalla/api/route/api-reference/#costing-models), [build configuration](https://github.com/valhalla/valhalla/blob/master/scripts/valhalla_build_config), [BSS mode-switch source](https://github.com/valhalla/valhalla/blob/master/src/thor/timedistancebssmatrix.cc#L1559-L1567)). | **Street paths only.** The documented model has no GBFS/TDX station-status input or per-request bikes/return-slots gate. Via must select eligible pickup and return stations from pinned Bike Availability, then ask Valhalla only for Walking and bicycle costs between those coordinates. Do not import volatile inventory into graph tiles or trust Valhalla's static `bikeshare` result as a valid YouBike Journey Option. |
| Pareto alternatives and ranking | Valhalla's route API can request alternates, but alternates are not supported for time-dependent routes. Its core is least-cost path search with runtime costing ([route alternatives](https://valhalla.github.io/valhalla/api/route/api-reference/#other-request-options), [dynamic costing](https://valhalla.github.io/valhalla/concepts/costing/dynamic-costing/)). | **Not Via's candidate engine.** Let McRangeRAPTOR and Via orchestration generate candidates. A Valhalla street path is one component; Via's deterministic dominance, generalized-cost, and diversity pipeline remains authoritative. |

## Integration with Rust Axum

Prefer Valhalla's HTTP boundary. Its official API uses REST-style JSON and also supports Protocol Buffers; it publishes an OpenAPI 3.1 description ([API index](https://valhalla.github.io/valhalla/api/), [OpenAPI documentation](https://valhalla.github.io/valhalla/api/openapi/)). Axum can therefore use a small typed client with strict deadlines, bounded concurrency, response-size limits, and circuit breaking. JSON is the simplest prototype; benchmark Protocol Buffers only if serialization becomes material.

Do not bind Via's domain types to Valhalla graph IDs. Graph IDs identify positions in a particular tile build and should be treated as revision-scoped adapter data. Via's stable Boarding Point and YouBike Station IDs remain provider-neutral, while their coordinates and any correlation result are versioned with the static planning artifact.

Recommended request pattern:

1. Axum pins the Network Snapshot and static graph revision.
2. It derives a bounded set of candidate Boarding Points and YouBike Stations from Via-owned spatial data.
3. It issues bounded Valhalla matrix requests for access, egress, Transfer, or bicycle movement.
4. McRangeRAPTOR and Via policy construct and filter Journey candidates.
5. Axum requests full Valhalla routes only for surviving street Legs when matrix output is insufficient for geometry/Guidance.
6. If Valhalla times out or returns inconsistent shared data, apply Via's existing local-Journey versus planner-wide failure rules; never silently substitute straight-line time.

This two-stage matrix-then-route flow avoids asking Valhalla to build complete multimodal Journey Options and limits detailed route work to candidates that can survive Via's live and timetable rules.

## Deployment and data lifecycle

Valhalla's own Docker images persist tiles, configuration, administrative/time-zone databases, and optional elevation in a mounted volume. They can build or load prebuilt tile archives, and the service thread count is an explicit resource control ([official Docker README](https://github.com/valhalla/valhalla/blob/master/docker/README.md)). The graph is a tiled, hierarchical binary structure designed to constrain memory, support cacheability, and permit regional extracts; tile archives can be memory-mapped and cache limits are configurable ([why tiles](https://valhalla.github.io/valhalla/concepts/why-tiles/), [graph reader/configuration source](https://github.com/valhalla/valhalla/blob/master/scripts/valhalla_build_config)).

For Via, tile building is offline publication work, not part of a Planning Request or the one-minute Live Status refresh. Build from a pinned Taiwan OSM extract, run quality fixtures, assign an immutable `street_graph_revision`, then deploy the artifact blue/green. Elevation is optional but valuable for bicycle-time and hill-cost quality; it increases the artifact/build pipeline and must be evaluated rather than assumed.

Operationally this adds a service with its own availability, CPU, memory, disk, deploy, and graph-refresh budget. The two-second p95 must include cross-service latency and queueing. Co-locate Valhalla with the selected Asia compute region or move the complete planner to the same long-running host if the extra network hop fails the latency gate.

## Licensing and attribution

Valhalla code is MIT-licensed ([Valhalla `COPYING`](https://github.com/valhalla/valhalla/blob/master/COPYING)). OpenStreetMap data is ODbL: public use requires credit to OpenStreetMap, an indication that the data is under ODbL, and share-alike obligations can apply to a publicly used Derivative Database ([OSM copyright and licence](https://www.openstreetmap.org/copyright), [ODbL 1.0](https://opendatacommons.org/licenses/odbl/1-0/)). Valhalla's own guidance gives the required attribution text as “© OpenStreetMap contributors” and says placement must be reasonable for the medium ([Valhalla attribution guidance](https://valhalla.github.io/valhalla/contributing/data/attribution/)).

Via should display OSM attribution anywhere OSM-derived route geometry or map-derived Guidance is presented, keep the licence link accessible, record the exact OSM extract/provenance for each graph revision, and obtain legal review before distributing graph tiles or combining OSM into a public derivative database. Apple basemap attribution does not replace attribution for OSM-derived routing data.

## Adoption gate

Adopt Valhalla only if a Taipei prototype demonstrates all of the following:

- Walking and bicycle paths are plausible at a curated set of origins, Boarding Points, Transfers, and YouBike Stations, including stairs, bridges, rivers, station entrances, one-way bicycle restrictions, and disconnected edges;
- bounded matrix calls cover Via's candidate counts and time orientations within the latency budget;
- graph correlation is stable enough to map back to Via's typed entities without persisting Valhalla IDs across graph revisions;
- the complete Axum→Valhalla→McRangeRAPTOR path meets the two-second p95 target under cold, warm, burst, and service-degradation scenarios;
- graph builds and blue/green publication are reproducible and rollbackable; and
- attribution and ODbL handling are approved.

Until that gate passes, record Valhalla as the preferred street-engine prototype, not an accepted production dependency.
