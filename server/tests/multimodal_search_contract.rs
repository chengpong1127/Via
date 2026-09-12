use time::macros::datetime;
use via_server::{
    live_status::FreshnessState,
    normalization::{
        ActionState, BikeAvailability, BikeShareStationId, BoardingPointId, ScheduledRunId,
        TransitServiceId,
    },
    planner::{
        StreetMetric,
        multimodal_graph::{CandidateEdge, CandidateNode, MultimodalGraph},
        search::{self, SearchLabel, SearchLimits},
    },
    transit::{TransitMode, TransitRide},
};

fn limits() -> SearchLimits {
    SearchLimits {
        max_labels_per_node: 8,
        max_transfer_count: 4,
        max_bike_share_expansions: 1,
        max_search_expansions: 100,
        max_journey_duration_seconds: 3_600,
        max_final_pareto_journeys: 8,
    }
}

fn label(arrival: i64, walking: u32, transfers: u32) -> SearchLabel {
    let time = datetime!(2026-08-31 10:00 UTC) + time::Duration::seconds(arrival);
    SearchLabel {
        node: CandidateNode::Destination,
        arrival_time: time,
        departure_time: datetime!(2026-08-31 10:00 UTC),
        walking_seconds: walking,
        bike_share_seconds: 0,
        transfer_count: transfers,
        bike_share_expansions: 0,
        required_freshness_states: Vec::new(),
        schedule_only: false,
        last_edge_was_transit: false,
        edge_ids: Vec::new(),
    }
}

#[test]
fn pareto_dominance_keeps_time_and_walking_tradeoffs() {
    let fast = label(38 * 60, 15 * 60, 2);
    let less_walking = label(40 * 60, 4 * 60, 1);
    let dominated = label(45 * 60, 20 * 60, 3);

    assert!(!search::dominates(&fast, &less_walking));
    assert!(!search::dominates(&less_walking, &fast));
    assert!(search::dominates(&less_walking, &dominated));
}

#[test]
fn search_composes_walking_transit_and_bikeshare_edges() {
    let mut graph = MultimodalGraph::default();
    let station = CandidateNode::BikeShareStation(BikeShareStationId::new("station"));
    let stop = CandidateNode::TransitStop(BoardingPointId::new("stop"));
    graph.add_edge(CandidateEdge::Walking {
        from: CandidateNode::Origin,
        to: station.clone(),
        metric: StreetMetric {
            duration_seconds: 60,
            distance_metres: 300,
        },
    });
    graph.add_edge(CandidateEdge::BikeShare {
        from: station.clone(),
        to: CandidateNode::BikeShareStation(BikeShareStationId::new("return")),
        pickup_station_id: BikeShareStationId::new("station"),
        return_station_id: BikeShareStationId::new("return"),
        metric: StreetMetric {
            duration_seconds: 300,
            distance_metres: 1_000,
        },
        availability: via_server::planner::multimodal_graph::BikeShareAvailability {
            pickup: BikeAvailability {
                station_id: BikeShareStationId::new("station"),
                rental_state: ActionState::Enabled,
                return_state: ActionState::Enabled,
                available_bikes: Some(3),
                available_return_slots: Some(3),
                source_change_at: None,
                provider_published_at: None,
                received_at: datetime!(2026-08-31 10:00 UTC),
            },
            return_station: BikeAvailability {
                station_id: BikeShareStationId::new("return"),
                rental_state: ActionState::Enabled,
                return_state: ActionState::Enabled,
                available_bikes: Some(3),
                available_return_slots: Some(3),
                source_change_at: None,
                provider_published_at: None,
                received_at: datetime!(2026-08-31 10:00 UTC),
            },
        },
    });
    graph.add_edge(CandidateEdge::Walking {
        from: CandidateNode::BikeShareStation(BikeShareStationId::new("return")),
        to: stop.clone(),
        metric: StreetMetric {
            duration_seconds: 60,
            distance_metres: 300,
        },
    });
    graph.add_edge(CandidateEdge::Transit {
        from: stop.clone(),
        to: CandidateNode::Destination,
        ride: via_server::planner::transit_timing::TimedTransitRide {
            ride: TransitRide {
                transit_mode: TransitMode::Metro,
                service_id: TransitServiceId::new("metro"),
                trip_id: ScheduledRunId::new("run"),
                board_stop_id: BoardingPointId::new("stop"),
                alight_stop_id: BoardingPointId::new("stop-destination"),
                scheduled_departure: datetime!(2026-08-31 10:10 UTC),
                scheduled_arrival: datetime!(2026-08-31 10:20 UTC),
            },
            departure: datetime!(2026-08-31 10:10 UTC),
            arrival: datetime!(2026-08-31 10:20 UTC),
            evidence: Vec::new(),
            required_states: vec![FreshnessState::NotApplicable],
            schedule_only: false,
        },
    });

    let result = search::search(&graph, datetime!(2026-08-31 10:00 UTC), None, &limits());
    assert_eq!(result.journeys.len(), 1);
    assert_eq!(result.journeys[0].bike_share_expansions, 1);
}

#[test]
fn transit_edges_require_an_explicit_transfer_edge() {
    let mut graph = MultimodalGraph::default();
    let first_stop = CandidateNode::TransitStop(BoardingPointId::new("first"));
    let second_stop = CandidateNode::TransitStop(BoardingPointId::new("second"));
    graph.add_edge(CandidateEdge::Walking {
        from: CandidateNode::Origin,
        to: first_stop.clone(),
        metric: StreetMetric {
            duration_seconds: 1,
            distance_metres: 1,
        },
    });
    let ride = |service_id: &str, trip_id: &str, board, alight, departure, arrival| {
        via_server::planner::transit_timing::TimedTransitRide {
            ride: TransitRide {
                transit_mode: TransitMode::Metro,
                service_id: TransitServiceId::new(service_id),
                trip_id: ScheduledRunId::new(trip_id),
                board_stop_id: BoardingPointId::new(board),
                alight_stop_id: BoardingPointId::new(alight),
                scheduled_departure: departure,
                scheduled_arrival: arrival,
            },
            departure,
            arrival,
            evidence: Vec::new(),
            required_states: vec![FreshnessState::NotApplicable],
            schedule_only: false,
        }
    };
    graph.add_edge(CandidateEdge::Transit {
        from: first_stop.clone(),
        to: second_stop.clone(),
        ride: ride(
            "first-service",
            "first-trip",
            "first",
            "second",
            datetime!(2026-08-31 10:01 UTC),
            datetime!(2026-08-31 10:05 UTC),
        ),
    });
    graph.add_edge(CandidateEdge::Transit {
        from: second_stop,
        to: CandidateNode::Destination,
        ride: ride(
            "second-service",
            "second-trip",
            "second",
            "destination",
            datetime!(2026-08-31 10:05 UTC),
            datetime!(2026-08-31 10:10 UTC),
        ),
    });

    let result = search::search(&graph, datetime!(2026-08-31 10:00 UTC), None, &limits());
    assert!(result.journeys.is_empty());
}

#[test]
fn zero_bikes_and_return_slots_make_a_bikeshare_edge_unusable() {
    let mut graph = MultimodalGraph::default();
    let station = CandidateNode::BikeShareStation(BikeShareStationId::new("station"));
    let return_station = CandidateNode::BikeShareStation(BikeShareStationId::new("return"));
    graph.add_edge(CandidateEdge::Walking {
        from: CandidateNode::Origin,
        to: station.clone(),
        metric: StreetMetric {
            duration_seconds: 1,
            distance_metres: 1,
        },
    });
    let availability = |bikes, slots| BikeAvailability {
        station_id: BikeShareStationId::new("station"),
        rental_state: ActionState::Enabled,
        return_state: ActionState::Enabled,
        available_bikes: Some(bikes),
        available_return_slots: Some(slots),
        source_change_at: None,
        provider_published_at: None,
        received_at: datetime!(2026-08-31 10:00 UTC),
    };
    graph.add_edge(CandidateEdge::BikeShare {
        from: station,
        to: return_station,
        pickup_station_id: BikeShareStationId::new("station"),
        return_station_id: BikeShareStationId::new("return"),
        metric: StreetMetric {
            duration_seconds: 1,
            distance_metres: 1,
        },
        availability: via_server::planner::multimodal_graph::BikeShareAvailability {
            pickup: availability(0, 2),
            return_station: availability(2, 0),
        },
    });
    let result = search::search(&graph, datetime!(2026-08-31 10:00 UTC), None, &limits());
    assert!(result.journeys.is_empty());
}

#[test]
fn search_expansion_limit_is_a_hard_bound() {
    let graph = MultimodalGraph::default();
    let mut bounded = limits();
    bounded.max_search_expansions = 0;
    let result = search::search(&graph, datetime!(2026-08-31 10:00 UTC), None, &bounded);
    assert!(result.journeys.is_empty());
    assert_eq!(result.stats.expansions, 0);
}
