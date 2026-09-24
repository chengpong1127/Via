# Public-transit correctness pass — 2026-09-23

Base: `main` at `e9f1a9c`. GTFS: `server/data/dev/gtfs.zip`, built network `server/data/dev/gtfs-transfers/transit-network.json` (99,836 stops, 7,167 services, 87,327 scheduled trips, 965,872 directional transfer edges). All requests depart **2026-09-24 10:00 +08:00**, within the archive's active calendar dates. Local Valhalla was unavailable; these live runs used the public Valhalla 3.9 endpoint for access, egress, and reconstruction, with the existing transfer edges preprocessed using local Valhalla tiles. The CLI used its live TDX provider and a 10-second Valhalla operation deadline to accommodate the remote service. No BikeShare mode was selected.

The final route trace includes direct walking options. Transit journey counts below include only options containing a transit leg. Counts and latencies are from these specific runs and vary with public provider response time and CPU contention. Snapshot summaries are `(successful positions, empty positions, failed positions, timing basis trip-entry counts)`; trip entries are not unique vehicles.

| Case | Transit journeys | First missing stage | Phase 1 | Snapshot | Phase 2 |
| --- | ---: | --- | ---: | ---: | ---: |
| Metro-only | 1 | None | 293 ms | 240 ms | 13 ms |
| Bus-only | 1 | None | 843 ms | 0 ms | 3 ms |
| Bus → Metro | 0 | Timing at first Bus boarding | 2,136 ms | 297 ms | 0 ms |
| Metro → Bus | 0 | Phase 1 corridor retention | 10,403 ms | 219 ms | 0 ms |
| TRA → Metro | 1 | None | 12,094 ms | 282 ms | 89 ms |
| THSR → Metro | 0 | Phase 1 safety bound | 89,529 ms | not entered | not entered |

## Metro-only: Taipei Main Station to Taipei 101

Works: one Metro journey via City Hall, followed by a long walk to Taipei 101. The raw GTFS has `TRTC_R03` (Taipei 101/World Trade Center) in `stops.txt` but no `TRTC_R03` rows in `stop_times.txt`; the built network therefore correctly excludes that unused station. The more direct Red Line arrival is unavailable in this archive.

**Access and egress.**

`discovered transit access and egress access=[("TRTC-BL12", [Metro], 282), ("TRTC-R10", [Metro], 282), ("TYMC-A1", [Metro], 359), ("TRTC-G14", [Metro], 685), ("TRTC-G13", [Metro], 719), ("TRTC-O11", [Metro], 1538), ("TRTC-O07", [Metro], 1641), ("TRTC-O08", [Metro], 1657)] egress=[("TRTC-BL18", [Metro], 873), ("TRTC-BR07", [Metro], 1588)]`

**Phase 1.**

`static Top-K corridor search static_search_states_expanded=598 static_service_scans=1898 static_positions_scanned=13914 static_complete_candidates_found=584 static_unique_corridors_found=584 static_corridors_retained=100 static_corridor_services=5 static_corridor_positions=22 static_corridor_bus_stops=0 static_search_latency_ms=293 kth_static_duration_seconds=Some(1815) exact_schedule_cost=100 derived_bus_cost=0 estimated_cost=0 mixed_cost=0 static_search_limit_reached=false max_legs=5`

**Timing snapshot.**

`timing snapshot positions_requested=22 snapshot_summary=(12, 10, 0, [(Schedule, 320)]) relevant_bus_stops=0 bike_share_enabled=false latency_ms=240`

**Phase 2.**

`temporal search initial_states=8 states_expanded=22 departure_lookups=53 effective_trips=90 transfer_expansions=14 final_candidates=1 positions_outside_corridor=154 positions_over_leg_budget=0 positions_without_departures=23 candidates_before_pareto=90 expansion_limit_reached=false latency_ms=13`

**Routing stages.**

`journey planning routing stages candidate_discovery_ms=734 multimodal_search_ms=554 transit_candidate_stops=10 reachable_boardings=22 reachable_services=5 realtime_stops_requested=0 temporal_departure_lookups=53 realtime_evidence_values=0 generated_candidates=2`

**Retained corridor service sequences (100 corridors; grouped identical sequences).**

```text
5 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
5 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
5 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=5 [exact_static_schedule]
5 × metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
5 × metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=5 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=4 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=4 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=5 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=5 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
4 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
2 × metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
2 × metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
2 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=4 [exact_static_schedule]
2 × metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-1::pattern=5 [exact_static_schedule]
2 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=4 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
2 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=4 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-1::pattern=5 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=4 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=5 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=5 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-1::pattern=4 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=5 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=5 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=5 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
1 × metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-1::pattern=5 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
```

**Bus provider.**

```text
logical needs=0
  realtime stops=0
  realtime batches=0
  HTTP requests=0
  rows received=0
  rows relevant=0
```

**Final route trace.**

```text
ROUTE 1
  depart=2026-09-24 10:00:00.0 +08:00:00 arrive=2026-09-24 10:32:29.0 +08:00:00 transfers=0 walking=1152s
  STREET walk origin -> 台北車站 (283s)
  TRANSIT service=metro:BL::direction=0::route=BL_BL-1::pattern=6 label="頂埔－南港展覽館" board=TRTC-BL12 alight=TRTC-BL18 (660s)
  STREET walk 市政府 -> destination (869s)
network revision: sha256:a06365fff68571d1f2c8e4029c687772da891e67ac88027815168c6d65c5f805
schedule revision: sha256:7b846aa245df9d0dee9609f161547989f52dcbd3cad9ee06adbd4caab682103c
service-condition mode: live

1. 33 min
   2026-09-24 10:00:00.0 +08:00:00 → 2026-09-24 10:32:29.0 +08:00:00
   timing basis: schedule/fallback
   Walk 5 min
   Metro: 頂埔－南港展覽館 (11 min)
   Walk 15 min
```

## Bus-only: Taipei Main Station to Ximen

Works: one scheduled Bus journey; direct walking ranks ahead.

**Access and egress.**

`discovered transit access and egress access=[("THB300475", [Bus], 233), ("TPE56105", [Bus], 233), ("THB237066", [Bus], 236), ("THB289792", [Bus], 236), ("THB289798", [Bus], 236), ("THB289800", [Bus], 236), ("THB289793", [Bus], 247), ("NWT158307", [Bus], 266), ("NWT158317", [Bus], 266), ("NWT158496", [Bus], 266), ("NWT204345", [Bus], 266), ("NWT212607", [Bus], 266), ("TPE12112", [Bus], 266), ("TPE15422", [Bus], 266), ("TPE20576", [Bus], 266), ("TPE219889", [Bus], 266), ("TPE22010", [Bus], 266), ("TPE35429", [Bus], 266), ("TPE36489", [Bus], 266), ("TPE39066", [Bus], 266), ("TPE39154", [Bus], 266), ("TPE40120", [Bus], 266), ("THB300514", [Bus], 314), ("THB300516", [Bus], 400)] egress=[("THB281514", [Bus], 56), ("NWT187383", [Bus], 58), ("NWT18844", [Bus], 58), ("TPE15246", [Bus], 58), ("TPE10656", [Bus], 61), ("TPE10798", [Bus], 61), ("TPE12379", [Bus], 61), ("TPE125338", [Bus], 61), ("TPE15317", [Bus], 61), ("TPE16430", [Bus], 61), ("TPE191234", [Bus], 61), ("TPE200331", [Bus], 61), ("TPE213087", [Bus], 61), ("TPE27088", [Bus], 61), ("TPE28170", [Bus], 61), ("TPE34728", [Bus], 61), ("TPE39043", [Bus], 61), ("TPE40591", [Bus], 61), ("TPE41195", [Bus], 61), ("TPE45007", [Bus], 61), ("TPE45328", [Bus], 61), ("TPE45424", [Bus], 61), ("TPE199201", [Bus], 61), ("THB268093", [Bus], 93)]`

**Phase 1.**

`static Top-K corridor search static_search_states_expanded=122 static_service_scans=357 static_positions_scanned=4180 static_complete_candidates_found=168 static_unique_corridors_found=168 static_corridors_retained=100 static_corridor_services=4 static_corridor_positions=4 static_corridor_bus_stops=1 static_search_latency_ms=843 kth_static_duration_seconds=Some(400) exact_schedule_cost=0 derived_bus_cost=0 estimated_cost=100 mixed_cost=0 static_search_limit_reached=false max_legs=5`

**Timing snapshot.**

`timing snapshot positions_requested=4 snapshot_summary=(2, 2, 0, [(Schedule, 9)]) relevant_bus_stops=1 bike_share_enabled=false latency_ms=0`

**Phase 2.**

`temporal search initial_states=24 states_expanded=24 departure_lookups=4 effective_trips=6 transfer_expansions=73 final_candidates=12 positions_outside_corridor=231 positions_over_leg_budget=0 positions_without_departures=2 candidates_before_pareto=78 expansion_limit_reached=false latency_ms=3`

**Routing stages.**

`journey planning routing stages candidate_discovery_ms=813 multimodal_search_ms=856 transit_candidate_stops=48 reachable_boardings=4 reachable_services=4 realtime_stops_requested=1 temporal_departure_lookups=4 realtime_evidence_values=0 generated_candidates=13`

**Retained corridor service sequences (100 corridors; grouped identical sequences).**

```text
28 × THB182001::direction=0 [optimistic_estimate]
24 × THB1820A1::direction=0 [optimistic_estimate]
24 × THB1820B1::direction=0 [optimistic_estimate]
24 × THB182101::direction=0 [optimistic_estimate]
```

**Bus provider.**

```text
logical needs=4
  realtime stops=0
  realtime batches=0
  HTTP requests=0
  rows received=0
  rows relevant=0
```

**Final route trace.**

```text
ROUTE 1
  depart=2026-09-24 10:00:00.0 +08:00:00 arrive=2026-09-24 10:18:49.0 +08:00:00 transfers=0 walking=1129s
  STREET walk origin -> destination (1129s)
ROUTE 2
  depart=2026-09-24 10:00:00.0 +08:00:00 arrive=2026-09-24 10:42:55.0 +08:00:00 transfers=0 walking=291s
  STREET walk origin -> 臺北車站(鄭州) (234s)
  STREET walk 臺北車站(鄭州) -> 臺北轉運站 (2s)
  TRANSIT service=THB182001::direction=0 label="1820" board=THB237066 alight=THB268093 (720s)
  STREET walk 捷運西門站 -> 捷運西門站 (25s)
  STREET walk 捷運西門站 -> destination (30s)
network revision: sha256:a06365fff68571d1f2c8e4029c687772da891e67ac88027815168c6d65c5f805
schedule revision: sha256:7b846aa245df9d0dee9609f161547989f52dcbd3cad9ee06adbd4caab682103c
service-condition mode: live

1. 19 min
   2026-09-24 10:00:00.0 +08:00:00 → 2026-09-24 10:18:49.0 +08:00:00
   timing basis: schedule/fallback
   Walk 19 min

2. 43 min
   2026-09-24 10:00:00.0 +08:00:00 → 2026-09-24 10:42:55.0 +08:00:00
   timing basis: schedule/fallback
   Walk 4 min
   Walk 1 min
   Bus: 1820 (12 min)
   Walk 1 min
   Walk 1 min
```

## Bus → Metro: Palace Museum to Ximen

Fails: retained corridors require Bus boarding, but the snapshot has no usable departure at the initial positions.

**Access and egress.**

`discovered transit access and egress access=[("TPE26637", [Bus], 4), ("TPE26811", [Bus], 4), ("TPE43070", [Bus], 4), ("TPE42568", [Bus], 7), ("NWT129633", [Bus], 151), ("TPE202123", [Bus], 151), ("TPE202188", [Bus], 151), ("TPE212405", [Bus], 151), ("TPE29044", [Bus], 151), ("TPE42856", [Bus], 151), ("TPE42936", [Bus], 151), ("TPE43044", [Bus], 151), ("TPE45304", [Bus], 151), ("TPE45396", [Bus], 151), ("TPE16713", [Bus], 262), ("TPE59211", [Bus], 262), ("TPE202083", [Bus], 412)] egress=[("TRTC-BL11", [Metro], 40), ("TRTC-G12", [Metro], 40), ("TPE10656", [Bus], 61), ("TPE10798", [Bus], 61), ("TPE12379", [Bus], 61), ("TPE125338", [Bus], 61), ("TPE15317", [Bus], 61), ("TPE16430", [Bus], 61), ("TPE191234", [Bus], 61), ("TPE200331", [Bus], 61), ("TPE213087", [Bus], 61), ("TPE27088", [Bus], 61), ("TPE199201", [Bus], 61), ("THB268093", [Bus], 93), ("TRTC-G13", [Metro], 663), ("TRTC-R09", [Metro], 679), ("TYMC-A1", [Metro], 950), ("TRTC-BL10", [Metro], 983)]`

**Phase 1.**

`static Top-K corridor search static_search_states_expanded=3342 static_service_scans=7362 static_positions_scanned=129625 static_complete_candidates_found=854 static_unique_corridors_found=854 static_corridors_retained=100 static_corridor_services=11 static_corridor_positions=12 static_corridor_bus_stops=7 static_search_latency_ms=2136 kth_static_duration_seconds=Some(983) exact_schedule_cost=0 derived_bus_cost=0 estimated_cost=0 mixed_cost=100 static_search_limit_reached=false max_legs=5`

**Timing snapshot.**

`timing snapshot positions_requested=12 snapshot_summary=(2, 10, 0, [(Schedule, 29)]) relevant_bus_stops=7 bike_share_enabled=false latency_ms=297`

**Phase 2.**

`temporal search initial_states=17 states_expanded=17 departure_lookups=2 effective_trips=0 transfer_expansions=2 final_candidates=0 positions_outside_corridor=20 positions_over_leg_budget=0 positions_without_departures=2 candidates_before_pareto=0 expansion_limit_reached=false latency_ms=0`

**Routing stages.**

`journey planning routing stages candidate_discovery_ms=766 multimodal_search_ms=2606 transit_candidate_stops=35 reachable_boardings=12 reachable_services=11 realtime_stops_requested=7 temporal_departure_lookups=2 realtime_evidence_values=0 generated_candidates=1`

**Retained corridor service sequences (100 corridors; grouped identical sequences).**

```text
4 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
4 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
4 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 [mixed]
4 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB180301::direction=0 → TPE161407::direction=1 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB1813J2::direction=1 → TPE161407::direction=1 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB1813J2::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB1813K2::direction=1 → TPE161407::direction=1 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB1813K2::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB180301::direction=0 → TPE161407::direction=1 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB1813J2::direction=1 → TPE161407::direction=1 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB1813J2::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB1813K2::direction=1 → TPE161407::direction=1 [mixed]
3 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB1813K2::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB180301::direction=0 → TPE161407::direction=1 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB1813J2::direction=1 → TPE161407::direction=1 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB1813J2::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB1813K2::direction=1 → TPE161407::direction=1 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE155130::direction=0 → THB1813K2::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB180301::direction=0 → TPE161407::direction=1 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB1813J2::direction=1 → TPE161407::direction=1 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB1813J2::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB1813K2::direction=1 → TPE161407::direction=1 [mixed]
3 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE162472::direction=0 → THB1813K2::direction=1 → metro:BL::direction=1::route=BL_BL-1::pattern=5 [mixed]
1 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 → THB180301::direction=0 → TPE161407::direction=1 [mixed]
1 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 → THB1813J2::direction=1 → TPE161407::direction=1 [mixed]
1 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 → THB1813K2::direction=1 → TPE161407::direction=1 [mixed]
1 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 → THB180301::direction=0 → TPE161407::direction=1 [mixed]
1 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 → THB1813J2::direction=1 → TPE161407::direction=1 [mixed]
1 × TPE159688::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 → THB1813K2::direction=1 → TPE161407::direction=1 [mixed]
1 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 → THB180301::direction=0 → TPE161407::direction=1 [mixed]
1 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 → THB1813J2::direction=1 → TPE161407::direction=1 [mixed]
1 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE157462::direction=1 → THB1813K2::direction=1 → TPE161407::direction=1 [mixed]
1 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 → THB180301::direction=0 → TPE161407::direction=1 [mixed]
1 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 → THB1813J2::direction=1 → TPE161407::direction=1 [mixed]
1 × TPE157468::direction=0 → metro:BR::direction=1::route=BR_BR-1 → TPE161407::direction=1 → THB1813K2::direction=1 → TPE161407::direction=1 [mixed]
```

**Bus provider.**

```text
logical needs=10
  realtime stops=7
  realtime batches=1
  HTTP requests=1
  rows received=0
  rows relevant=0
```

**Final route trace.**

```text
ROUTE 1
  depart=2026-09-24 10:00:00.0 +08:00:00 arrive=2026-09-24 11:53:53.0 +08:00:00 transfers=0 walking=6833s
  STREET walk origin -> destination (6833s)
network revision: sha256:a06365fff68571d1f2c8e4029c687772da891e67ac88027815168c6d65c5f805
schedule revision: sha256:7b846aa245df9d0dee9609f161547989f52dcbd3cad9ee06adbd4caab682103c
service-condition mode: live

1. 114 min
   2026-09-24 10:00:00.0 +08:00:00 → 2026-09-24 11:53:53.0 +08:00:00
   timing basis: schedule/fallback
   Walk 114 min
```

## Metro → Bus: Taipei Main Station to Palace Museum

Fails in Phase 1: all 100 retained corridors are Bus-only despite Metro access being discovered.

**Access and egress.**

`discovered transit access and egress access=[("NWT158307", [Bus], 266), ("NWT158317", [Bus], 266), ("NWT158496", [Bus], 266), ("NWT204345", [Bus], 266), ("NWT212607", [Bus], 266), ("TPE12112", [Bus], 266), ("TPE15422", [Bus], 266), ("TPE20576", [Bus], 266), ("TPE219889", [Bus], 266), ("TPE22010", [Bus], 266), ("TPE35429", [Bus], 266), ("TPE36489", [Bus], 266), ("TRTC-BL12", [Metro], 282), ("TRTC-R10", [Metro], 282), ("TYMC-A1", [Metro], 359), ("TRTC-G14", [Metro], 685), ("TRTC-G13", [Metro], 719)] egress=[("TPE26637", [Bus], 4), ("TPE26811", [Bus], 4), ("TPE43070", [Bus], 4), ("TPE42568", [Bus], 7), ("NWT129633", [Bus], 123), ("TPE202123", [Bus], 123), ("TPE202188", [Bus], 123), ("TPE212405", [Bus], 123), ("TPE29044", [Bus], 123), ("TPE42856", [Bus], 123), ("TPE42936", [Bus], 123), ("TPE43044", [Bus], 123), ("TPE45304", [Bus], 123), ("TPE45396", [Bus], 123), ("TPE16713", [Bus], 226), ("TPE59211", [Bus], 226), ("TPE202083", [Bus], 345)]`

**Phase 1.**

`static Top-K corridor search static_search_states_expanded=3264 static_service_scans=5770 static_positions_scanned=278299 static_complete_candidates_found=860 static_unique_corridors_found=860 static_corridors_retained=100 static_corridor_services=17 static_corridor_positions=19 static_corridor_bus_stops=12 static_search_latency_ms=10403 kth_static_duration_seconds=Some(967) exact_schedule_cost=0 derived_bus_cost=0 estimated_cost=0 mixed_cost=100 static_search_limit_reached=false max_legs=5`

**Timing snapshot.**

`timing snapshot positions_requested=19 snapshot_summary=(1, 18, 0, [(Schedule, 16)]) relevant_bus_stops=12 bike_share_enabled=false latency_ms=219`

**Phase 2.**

`temporal search initial_states=17 states_expanded=17 departure_lookups=1 effective_trips=0 transfer_expansions=0 final_candidates=0 positions_outside_corridor=42 positions_over_leg_budget=0 positions_without_departures=1 candidates_before_pareto=0 expansion_limit_reached=false latency_ms=0`

**Routing stages.**

`journey planning routing stages candidate_discovery_ms=713 multimodal_search_ms=10835 transit_candidate_stops=34 reachable_boardings=19 reachable_services=17 realtime_stops_requested=12 temporal_departure_lookups=1 realtime_evidence_values=0 generated_candidates=1`

**Retained corridor service sequences (100 corridors; grouped identical sequences).**

```text
4 × TPE16112::direction=0 → TPE158754::direction=0 → THB180102::direction=1 → TPE112110::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112170::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE157454::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE157455::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112170::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE157454::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE157455::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE159509::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112170::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE159509::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE157454::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE159509::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE157455::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE159508::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112170::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE159508::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE157454::direction=0 [mixed]
4 × TPE16112::direction=0 → TPE159508::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE157455::direction=0 [mixed]
3 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE157453::direction=0 → TPE112170::direction=0 [mixed]
3 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE157453::direction=0 → TPE157454::direction=0 [mixed]
2 × TPE16112::direction=0 → TPE157855::direction=0 → THB180102::direction=1 → TPE112110::direction=0 [mixed]
2 × TPE16112::direction=0 → TPE159508::direction=0 → THB180102::direction=1 → TPE112110::direction=0 [mixed]
2 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112110::direction=0 [mixed]
2 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112530::direction=0 [mixed]
2 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE159512::direction=0 [mixed]
2 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112110::direction=0 [mixed]
2 × TPE16112::direction=0 → TPE159509::direction=0 → THB180102::direction=1 → TPE112110::direction=0 [mixed]
2 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112530::direction=0 [mixed]
2 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE159512::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE159509::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112110::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE159508::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112110::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE159509::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112530::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE159509::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE159512::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE157610::direction=1 → TPE112110::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE157453::direction=0 → TPE112110::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE159508::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE112530::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE159508::direction=0 → THB900602::direction=1 → TPE102720::direction=0 → TPE159512::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE157610::direction=1 → TPE112530::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE157610::direction=1 → TPE159512::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE102750::direction=0 → TPE112110::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE157453::direction=0 → TPE112530::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE157453::direction=0 → TPE159512::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE157610::direction=1 → TPE112110::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE157453::direction=0 → TPE112110::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE102750::direction=0 → TPE112530::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE157855::direction=0 → THB900602::direction=1 → TPE102750::direction=0 → TPE159512::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE157610::direction=1 → TPE112530::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE157610::direction=1 → TPE159512::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE102750::direction=0 → TPE112110::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE157453::direction=0 → TPE112530::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE157453::direction=0 → TPE159512::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE102750::direction=0 → TPE112530::direction=0 [mixed]
1 × TPE16112::direction=0 → TPE158754::direction=0 → THB900602::direction=1 → TPE102750::direction=0 → TPE159512::direction=0 [mixed]
```

**Bus provider.**

```text
logical needs=19
  realtime stops=12
  realtime batches=1
  HTTP requests=1
  rows received=0
  rows relevant=0
```

**Final route trace.**

```text
ROUTE 1
  depart=2026-09-24 10:00:00.0 +08:00:00 arrive=2026-09-24 11:44:21.0 +08:00:00 transfers=0 walking=6261s
  STREET walk origin -> destination (6261s)
network revision: sha256:a06365fff68571d1f2c8e4029c687772da891e67ac88027815168c6d65c5f805
schedule revision: sha256:7b846aa245df9d0dee9609f161547989f52dcbd3cad9ee06adbd4caab682103c
service-condition mode: live

1. 105 min
   2026-09-24 10:00:00.0 +08:00:00 → 2026-09-24 11:44:21.0 +08:00:00
   timing basis: schedule/fallback
   Walk 105 min
```

## TRA → Metro: Taoyuan TRA to Taipei 101

Works: one scheduled TRA → Metro journey; direct walking is a second option.

**Access and egress.**

`discovered transit access and egress access=[("TRA-1080", [Rail], 0)] egress=[("TRTC-BL18", [Metro], 873), ("TRTC-BR07", [Metro], 1588)]`

**Phase 1.**

`static Top-K corridor search static_search_states_expanded=3204 static_service_scans=50785 static_positions_scanned=649708 static_complete_candidates_found=3878 static_unique_corridors_found=3878 static_corridors_retained=100 static_corridor_services=49 static_corridor_positions=80 static_corridor_bus_stops=0 static_search_latency_ms=12094 kth_static_duration_seconds=Some(3192) exact_schedule_cost=100 derived_bus_cost=0 estimated_cost=0 mixed_cost=0 static_search_limit_reached=false max_legs=5`

**Timing snapshot.**

`timing snapshot positions_requested=80 snapshot_summary=(30, 50, 0, [(Schedule, 534)]) relevant_bus_stops=0 bike_share_enabled=false latency_ms=282`

**Phase 2.**

`temporal search initial_states=1 states_expanded=102 departure_lookups=306 effective_trips=477 transfer_expansions=101 final_candidates=1 positions_outside_corridor=1435 positions_over_leg_budget=0 positions_without_departures=123 candidates_before_pareto=308 expansion_limit_reached=false latency_ms=89`

**Routing stages.**

`journey planning routing stages candidate_discovery_ms=639 multimodal_search_ms=12667 transit_candidate_stops=3 reachable_boardings=80 reachable_services=49 realtime_stops_requested=0 temporal_departure_lookups=306 realtime_evidence_values=0 generated_candidates=2`

**Retained corridor service sequences (100 corridors; grouped identical sequences).**

```text
10 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
8 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
5 × TRA_108_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
4 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=5 [exact_static_schedule]
4 × TRA_108_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
2 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=2 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
2 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=4 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
2 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=5 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
2 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
2 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=3 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
2 × TRA_5466_0_1132::direction=0 → TRA_1006_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
2 × TRA_108_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=6 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_108_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_110_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_110_0_110H::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_116_0_110K::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_120_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_136_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_144_0_110K::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_144_0_110M::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_146_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_146_0_110K::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_154_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_168_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_172_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_172_0_110H::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_194_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
1 × TRA_472_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5466_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_1006_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_1048_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_112_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_112_0_1109::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_114_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_114_0_1109::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_118_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_126_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_128_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_130_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_134_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_142_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_150_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_150_0_1109::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_178_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_180_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_2008_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_2042_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_2046_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_288_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-2 → metro:BL::direction=0::route=BL_BL-1::pattern=4 [exact_static_schedule]
1 × TRA_5114_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5122_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5142_0_1108::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5150_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5466_0_1132::direction=0 → TRA_1048_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5466_0_1132::direction=0 → TRA_144_0_110K::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5466_0_1132::direction=0 → TRA_144_0_110M::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5466_0_1132::direction=0 → TRA_178_0_1107::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5466_0_1132::direction=0 → TRA_2046_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5466_0_1132::direction=0 → TRA_4006_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5466_0_1132::direction=0 → TRA_4046_0_1132::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_5466_0_1132::direction=0 → TRA_472_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_108_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=2 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_108_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=4 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_108_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=5 → metro:BL::direction=0::route=BL_BL-2 [exact_static_schedule]
1 × TRA_108_0_110G::direction=0 → metro:BL::direction=0::route=BL_BL-1::pattern=6 [exact_static_schedule]
```

**Bus provider.**

```text
logical needs=0
  realtime stops=0
  realtime batches=0
  HTTP requests=0
  rows received=0
  rows relevant=0
```

**Final route trace.**

```text
ROUTE 1
  depart=2026-09-24 10:00:00.0 +08:00:00 arrive=2026-09-24 11:08:29.0 +08:00:00 transfers=1 walking=1028s
  STREET walk origin -> 桃園 (0s)
  TRANSIT service=TRA_108_0_110G::direction=0 label="潮州-七堵" board=TRA-1080 alight=TRA-1020 (1140s)
  STREET walk 板橋 -> 板橋 (159s)
  TRANSIT service=metro:BL::direction=0::route=BL_BL-1::pattern=6 label="頂埔－南港展覽館" board=TRTC-BL07 alight=TRTC-BL18 (1440s)
  STREET walk 市政府 -> destination (869s)
ROUTE 2
  depart=2026-09-24 10:00:00.0 +08:00:00 arrive=2026-09-24 15:56:47.0 +08:00:00 transfers=0 walking=21407s
  STREET walk origin -> destination (21407s)
network revision: sha256:a06365fff68571d1f2c8e4029c687772da891e67ac88027815168c6d65c5f805
schedule revision: sha256:7b846aa245df9d0dee9609f161547989f52dcbd3cad9ee06adbd4caab682103c
service-condition mode: live

1. 69 min
   2026-09-24 10:00:00.0 +08:00:00 → 2026-09-24 11:08:29.0 +08:00:00
   timing basis: schedule/fallback
   Walk 0 min
   Rail: 潮州-七堵 (19 min)
   Walk 3 min
   Metro: 頂埔－南港展覽館 (24 min)
   Walk 15 min

2. 357 min
   2026-09-24 10:00:00.0 +08:00:00 → 2026-09-24 15:56:47.0 +08:00:00
   timing basis: schedule/fallback
   Walk 357 min
```

## THSR → Metro: Taoyuan THSR to Taipei 101

Fails in Phase 1: the 20,000-state safety bound is reached before a complete corridor.

**Access and egress.**

`discovered transit access and egress access=[("THSR-1020", [HighSpeedRail], 0), ("TYMC-A18", [Metro], 149)] egress=[("TRTC-BL18", [Metro], 873), ("TRTC-BR07", [Metro], 1588)]`

**Phase 1.**

`static Top-K corridor search static_search_states_expanded=20000 static_service_scans=369496 static_positions_scanned=1388352 static_complete_candidates_found=0 static_unique_corridors_found=0 static_corridors_retained=0 static_corridor_services=0 static_corridor_positions=0 static_corridor_bus_stops=0 static_search_latency_ms=89529 kth_static_duration_seconds=None exact_schedule_cost=0 derived_bus_cost=0 estimated_cost=0 mixed_cost=0 static_search_limit_reached=true max_legs=5`

**Timing snapshot.** Not entered.

**Phase 2.** Not entered.

**Routing stages.**

`journey planning routing stages candidate_discovery_ms=782 multimodal_search_ms=0 transit_candidate_stops=4 reachable_boardings=0 reachable_services=0 realtime_stops_requested=0 temporal_departure_lookups=0 realtime_evidence_values=0 generated_candidates=1`

**Retained corridor service sequences (0 corridors; grouped identical sequences).**

None.

**Bus provider.**

```text
logical needs=0
  realtime stops=0
  realtime batches=0
  HTTP requests=0
  rows received=0
  rows relevant=0
```

**Final route trace.**

```text
ROUTE 1
  depart=2026-09-24 10:00:00.0 +08:00:00 arrive=2026-09-24 18:36:42.0 +08:00:00 transfers=0 walking=31002s
  STREET walk origin -> destination (31002s)
network revision: sha256:a06365fff68571d1f2c8e4029c687772da891e67ac88027815168c6d65c5f805
schedule revision: sha256:7b846aa245df9d0dee9609f161547989f52dcbd3cad9ee06adbd4caab682103c
service-condition mode: live

1. 517 min
   2026-09-24 10:00:00.0 +08:00:00 → 2026-09-24 18:36:42.0 +08:00:00
   timing basis: schedule/fallback
   Walk 517 min
```

## Diagnosis and readiness

Changes made:

- `server/src/domain/transit.rs`: use validated `DerivedBusTravelTime.adjacent_seconds` for Bus realtime downstream estimates when a complete scheduled trip is absent; retain the existing 120-second generic estimate when no derived data exists.
- `server/src/routing/access.rs`: select nearby stops in rounds across allowed modes before using remaining slots, so co-located Bus IDs cannot consume every slot ahead of another Metro entrance. The 24-stop safety limit is unchanged.
- `server/tests/timing/bus.rs` and `server/src/routing/access.rs` tests: cover derived Bus timing anchored to TDX boarding ETA and multiple Metro entrances amid dense Bus stops. Existing Metro, Bus, transfer, and mixed-mode tests remain in the full suite.
- `server/src/routing/service.rs`, `planner.rs`, `temporal.rs`, `domain/transit_timing.rs`, and `bin/via-route.rs`: expose access lists, all retained corridor sequences, snapshot outcomes, Phase 2 pruning counts, and a configurable CLI Valhalla timeout for repeatable remote diagnostics. The server defaults and search safety limits are unchanged.
- `CONTEXT.md`, `docs/dev-tooling.md`, and `server/src/data/gtfs.rs`: describe the existing intended derived-timing semantics accurately.

Two concrete correctness defects were fixed: derived Bus segment costs were ignored by the realtime downstream timing index, and multimode nearby-stop selection allowed a dense set of Bus stop IDs to exclude other Metro entrances. Tests cover both. The first defect cannot be demonstrated by the fixed future departure because live city Bus rows were empty; its TDX fixture regression exercises the exact fallback path.

The two mixed Bus routes remain incomplete. Bus → Metro retains mixed corridors but has no departure at the initial selected Bus boarding positions; live TDX made one request and returned zero rows, while incomplete Bus stop times have no static schedule by design. Metro → Bus retains 100 Bus-only corridors, so the expected Metro-first sequence disappears in Phase 1 even after Red Line access is available. This is candidate starvation from zero-wait Bus estimates, not a justification to increase K blindly.

Metro-only reaches the destination area but uses City Hall and 869 seconds of egress walking because the source GTFS does not contain trip stop times for `TRTC_R03`. This is an archive coverage gap, not a GTFS ID normalization bug.

THSR → Metro remains blocked by Phase 1 search expansion (20,000 states, 1,388,352 position scans in the live run). No snapshot or temporal search is attempted. The dominant measured runtime is Phase 1: approximately 2.1 seconds for Bus → Metro, 10.4 seconds for Metro → Bus, 12.1 seconds for TRA → Metro, and 89.5 seconds for the THSR failure under concurrent build load. Access/egress takes about 0.7–0.8 seconds with public Valhalla; snapshot and Phase 2 are smaller in successful cases.

**Ready for Phase 2 algorithm comparison: no.** First resolve the Phase 1 corridor starvation and THSR search-bound failure, then rerun mixed mode cases with a current daytime TDX window or a deterministic replay of TDX rows.

Verification after the changes: `cargo fmt --manifest-path server/Cargo.toml --check`, `cargo clippy --manifest-path server/Cargo.toml --all-targets --all-features`, and `cargo test --manifest-path server/Cargo.toml --all` all passed.
