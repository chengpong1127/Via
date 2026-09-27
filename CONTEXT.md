# Via current architecture

Via plans multimodal journeys in Taipei and New Taipei using an immutable
GTFS-built transit network, request-local TDX realtime, Valhalla street
routing, and provider-neutral API models.

## Request flow

```text
PlanningRequest
  → RoutingService
  ├─ direct walking
  └─ mode-aware transit access / egress
       → TransitPlanner
       → Phase 1: bounded static Top-K structural corridors
       → one request-local TimingSnapshot
       → optional BikeAvailabilitySnapshot
       → Phase 2: fixed corridor evaluation
            ├─ Transit: equivalent Pattern and departure choices
            └─ Surface transfer: Walk or YouBike
       → reconstruct → rank → JourneyOption
```

`TransitPlanner` is the sole concrete transit planner. Phase 2 evaluates each
retained Phase 1 corridor in order. It cannot search transit topology, add an
anchor, or combine steps from different corridors. There is no routing
algorithm interface, Trip-Based implementation, route graph, or wrapper
planning service.

## Static corridor discovery

`TransitNetwork` stores Via Routes, ordered Patterns, Trips, Service Calendars,
Frequencies, and directional Transfers. `TransitTopologyIndex` stores
`PatternPosition(pattern_id, stop_index)` occurrences by boarding point, so
loop routes remain unambiguous. The static search uses one global K across all
selected transit modes. It ranks structurally distinct corridors using access
and egress walking, Via-preprocessed transfer walking, exact static segment
times where available, derived Bus segment costs, and documented optimistic
geometry estimates otherwise. Bike availability never influences Phase 1.

Sparse Bus stop times do not become fabricated `ScheduledTrip` values. When
terminal runtime is sufficient, preprocessing derives per-segment Bus costs
from stop or shape distances; otherwise it uses geometric estimates. These
costs rank Phase 1 candidates and are not timetable data.

`CandidateCorridor` contains retained corridors, equivalent transit legs,
timing positions, and selected Bus stops. Equivalent legs share mode and
boarding/alighting points; Phase 2 can choose any matching Pattern and
departure while preserving the retained structural steps.

## Request-local evaluation

The planner loads one timing snapshot for the retained timing positions before
calling the synchronous, provider-free corridor evaluator. Bus realtime is
fetched by unique stop and scope, then filtered locally. Other modes keep their
provider-native batching and static fallback behavior.

When BikeShare is selected, one availability snapshot is loaded alongside
transit timing. The evaluator considers walking and feasible YouBike choices
only for retained surface transfers. A YouBike choice requires a fresh,
serviceable pickup with a bike and a return station with a dock. It includes
walks to and from the stations and a bicycle route. Candidate station choices
are bounded to the three nearest usable pickups and three nearest usable
dropoffs per transfer anchor, so each structural movement evaluates at most
nine pairs. BikeShare never adds transit connectivity, changes an anchor, or
creates a direct BikeShare-only route. Direct walking remains walking-only.

Static transfer edges are directional facts in `TransitNetwork.transfer_edges`.
`TransferShortcutIndex` provides incoming and outgoing lookups; there is no
async transfer resolver.

## Static and live data

`via-data` reads the supplied local GTFS archive and does not download or
refresh it. GTFS is the only static transit source. TDX IDs are normalized to
existing Via IDs when their formats map directly; unmapped IDs are reported.
The server has no database or identity-reconciliation layer.

GTFS does not contain a YouBike station catalog. To build the static station
metadata, `via-data build` can take local TDX Taipei and New Taipei
`Bike/Station/City` JSON snapshots. It maps their station IDs to the same
city-scoped Via IDs used by availability, then precomputes bounded directional
walking links with Valhalla. Live bike and dock counts, service state, and
freshness come from TDX request-time availability. Without station snapshots,
the generated BikeShare artifact remains empty.

Keep the dependency direction `domain ← routing ← app/api`. Providers
implement domain-facing interfaces. iOS remains feature-oriented
(`App`, `Components`, `Domain`, `Features`, `Resources`, `Services`), with
production planning through `JourneyPlanner → HTTPJourneyPlanner → APIClient`.

## Essential commands

```bash
cargo fmt --manifest-path server/Cargo.toml --check
cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features
cargo test --manifest-path server/Cargo.toml --all
```

See [`docs/dev-tooling.md`](docs/dev-tooling.md) for data and route-debug
commands.
