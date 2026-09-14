use time::macros::datetime;
use via_server::{
    domain::{
        ids::{BikeShareStationId, BoardingPointId, ScheduledRunId, TransitServiceId},
        realtime::{ActionState, BikeAvailability, FreshnessState},
        transit::{TransitMode, TransitRide},
    },
    routing::TimingBasis,
    routing::{
        StreetMetric,
        enrich::TimedTransitRide,
        graph::{RouteEdge, RouteGraph, RouteNode},
        search::{self, SearchLabel, SearchLimits},
    },
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
        node: RouteNode::Destination,
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
    let mut graph = RouteGraph::default();
    let station = RouteNode::BikeShareStation(BikeShareStationId::new("station"));
    let stop = RouteNode::TransitStop(BoardingPointId::new("stop"));
    graph.add_edge(RouteEdge::Walk {
        from: RouteNode::Origin,
        to: station.clone(),
        metric: StreetMetric {
            duration_seconds: 60,
            distance_metres: 300,
        },
    });
    graph.add_edge(RouteEdge::Bike {
        from: station.clone(),
        to: RouteNode::BikeShareStation(BikeShareStationId::new("return")),
        pickup_station_id: BikeShareStationId::new("station"),
        return_station_id: BikeShareStationId::new("return"),
        metric: StreetMetric {
            duration_seconds: 300,
            distance_metres: 1_000,
        },
        availability: via_server::routing::graph::BikeEdgeAvailability {
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
    graph.add_edge(RouteEdge::Walk {
        from: RouteNode::BikeShareStation(BikeShareStationId::new("return")),
        to: stop.clone(),
        metric: StreetMetric {
            duration_seconds: 60,
            distance_metres: 300,
        },
    });
    graph.add_edge(RouteEdge::Transit {
        from: stop.clone(),
        to: RouteNode::Destination,
        ride: via_server::routing::enrich::TimedTransitRide {
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
            timing_basis: TimingBasis::RealtimePrediction,
        },
    });

    let result = search::search(&graph, datetime!(2026-08-31 10:00 UTC), None, &limits());
    assert_eq!(result.journeys.len(), 1);
    assert_eq!(result.journeys[0].bike_share_expansions, 1);
}

#[test]
fn transit_edges_require_an_explicit_transfer_edge() {
    let mut graph = RouteGraph::default();
    let first_stop = RouteNode::TransitStop(BoardingPointId::new("first"));
    let second_stop = RouteNode::TransitStop(BoardingPointId::new("second"));
    graph.add_edge(RouteEdge::Walk {
        from: RouteNode::Origin,
        to: first_stop.clone(),
        metric: StreetMetric {
            duration_seconds: 1,
            distance_metres: 1,
        },
    });
    let ride = |service_id: &str, trip_id: &str, board, alight, departure, arrival| {
        via_server::routing::enrich::TimedTransitRide {
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
            timing_basis: TimingBasis::RealtimePrediction,
        }
    };
    graph.add_edge(RouteEdge::Transit {
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
    graph.add_edge(RouteEdge::Transit {
        from: second_stop,
        to: RouteNode::Destination,
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
    let mut graph = RouteGraph::default();
    let station = RouteNode::BikeShareStation(BikeShareStationId::new("station"));
    let return_station = RouteNode::BikeShareStation(BikeShareStationId::new("return"));
    graph.add_edge(RouteEdge::Walk {
        from: RouteNode::Origin,
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
    graph.add_edge(RouteEdge::Bike {
        from: station,
        to: return_station,
        pickup_station_id: BikeShareStationId::new("station"),
        return_station_id: BikeShareStationId::new("return"),
        metric: StreetMetric {
            duration_seconds: 1,
            distance_metres: 1,
        },
        availability: via_server::routing::graph::BikeEdgeAvailability {
            pickup: availability(0, 2),
            return_station: availability(2, 0),
        },
    });
    let result = search::search(&graph, datetime!(2026-08-31 10:00 UTC), None, &limits());
    assert!(result.journeys.is_empty());
}

#[test]
fn search_expansion_limit_is_a_hard_bound() {
    let graph = RouteGraph::default();
    let mut bounded = limits();
    bounded.max_search_expansions = 0;
    let result = search::search(&graph, datetime!(2026-08-31 10:00 UTC), None, &bounded);
    assert!(result.journeys.is_empty());
    assert_eq!(result.stats.expansions, 0);
}

fn transit_edge(
    service_id: &str,
    from: &str,
    to: &str,
    departure: time::OffsetDateTime,
    arrival: time::OffsetDateTime,
) -> RouteEdge {
    RouteEdge::Transit {
        from: RouteNode::TransitStop(BoardingPointId::new(from)),
        to: if to == "destination" {
            RouteNode::Destination
        } else {
            RouteNode::TransitStop(BoardingPointId::new(to))
        },
        ride: TimedTransitRide {
            ride: TransitRide {
                transit_mode: TransitMode::Bus,
                service_id: TransitServiceId::new(service_id),
                trip_id: ScheduledRunId::new(format!("trip-{service_id}-{from}-{to}")),
                board_stop_id: BoardingPointId::new(from),
                alight_stop_id: BoardingPointId::new(to),
                scheduled_departure: departure,
                scheduled_arrival: arrival,
            },
            departure,
            arrival,
            evidence: Vec::new(),
            required_states: Vec::new(),
            timing_basis: TimingBasis::RealtimePrediction,
        },
    }
}

#[test]
fn same_service_continuation_is_one_leg_without_a_transfer() {
    let start = datetime!(2026-08-31 10:00 UTC);
    let mut graph = RouteGraph::default();
    graph.add_edge(RouteEdge::Walk {
        from: RouteNode::Origin,
        to: RouteNode::TransitStop(BoardingPointId::new("a")),
        metric: StreetMetric {
            duration_seconds: 60,
            distance_metres: 100,
        },
    });
    graph.add_edge(transit_edge(
        "bus-a",
        "a",
        "b",
        start + time::Duration::minutes(2),
        start + time::Duration::minutes(8),
    ));
    graph.add_edge(transit_edge(
        "bus-a",
        "b",
        "destination",
        start + time::Duration::minutes(8),
        start + time::Duration::minutes(12),
    ));

    let result = search::search(&graph, start, None, &limits());
    assert_eq!(result.journeys.len(), 1);
    assert_eq!(result.journeys[0].transfer_count, 0);
}

#[test]
fn changing_service_after_a_connector_counts_one_transfer() {
    let start = datetime!(2026-08-31 10:00 UTC);
    let mut graph = RouteGraph::default();
    graph.add_edge(RouteEdge::Walk {
        from: RouteNode::Origin,
        to: RouteNode::TransitStop(BoardingPointId::new("a")),
        metric: StreetMetric {
            duration_seconds: 60,
            distance_metres: 100,
        },
    });
    graph.add_edge(transit_edge(
        "bus-a",
        "a",
        "b",
        start + time::Duration::minutes(2),
        start + time::Duration::minutes(8),
    ));
    graph.add_edge(RouteEdge::Walk {
        from: RouteNode::TransitStop(BoardingPointId::new("b")),
        to: RouteNode::TransitStop(BoardingPointId::new("c")),
        metric: StreetMetric {
            duration_seconds: 60,
            distance_metres: 0,
        },
    });
    graph.add_edge(transit_edge(
        "metro-c",
        "c",
        "destination",
        start + time::Duration::minutes(10),
        start + time::Duration::minutes(20),
    ));

    let result = search::search(&graph, start, None, &limits());
    assert_eq!(result.journeys.len(), 1);
    assert_eq!(result.journeys[0].transfer_count, 1);
}
