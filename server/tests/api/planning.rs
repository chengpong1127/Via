use serde_json::json;
use time::macros::{date, datetime};
use via_server::{
    api::{
        DepartureSetting, PlanningMode, PlanningRequest, RequestValidationError,
        normalize_to_taipei,
    },
    domain::realtime::{FreshnessPolicy, FreshnessState, LiveStatusTimes},
};

#[test]
fn client_offset_is_normalized_before_service_day_lookup() {
    let setting = DepartureSetting::DepartAt {
        time: datetime!(2026-08-31 17:30 UTC),
    };
    let effective = setting.effective_search_time(datetime!(2026-09-01 00:00 UTC));
    assert_eq!(effective, datetime!(2026-09-01 01:30 +08:00));
    assert_eq!(
        setting.service_day(
            datetime!(2026-09-01 00:00 UTC),
            time::Time::from_hms(4, 0, 0).unwrap()
        ),
        date!(2026 - 08 - 31)
    );
    assert_eq!(
        normalize_to_taipei(datetime!(2026-08-31 17:30 UTC)).offset(),
        time::UtcOffset::from_hms(8, 0, 0).unwrap()
    );
}

#[test]
fn depart_now_depart_at_and_arrive_by_resolve_in_taipei_time() {
    let now = datetime!(2026-08-31 12:00 UTC);
    let depart_now = DepartureSetting::DepartNow.resolve(now, 600);
    assert_eq!(
        depart_now,
        (
            datetime!(2026-08-31 20:00 +08:00),
            datetime!(2026-08-31 20:10 +08:00)
        )
    );

    let depart_at = DepartureSetting::DepartAt {
        time: datetime!(2026-08-31 12:00 UTC),
    }
    .resolve(now, 600);
    assert_eq!(
        depart_at,
        (
            datetime!(2026-08-31 20:00 +08:00),
            datetime!(2026-08-31 20:10 +08:00)
        )
    );

    let arrive_by = DepartureSetting::ArriveBy {
        time: datetime!(2026-08-31 12:00 UTC),
    }
    .resolve(now, 600);
    assert_eq!(
        arrive_by,
        (
            datetime!(2026-08-31 19:50 +08:00),
            datetime!(2026-08-31 20:00 +08:00)
        )
    );
}

#[test]
fn planning_request_accepts_the_provider_neutral_contract() {
    let request: PlanningRequest = serde_json::from_value(json!({
        "origin": {
            "coordinate": {"latitude": 25.04776, "longitude": 121.51706},
            "display_text": "Taipei Main Station"
        },
        "destination": {
            "coordinate": {"latitude": 25.03396, "longitude": 121.56447},
            "display_text": "Taipei 101"
        },
        "selected_modes": ["bus", "metro", "bike_share"],
        "departure_setting": {"type": "depart_now"}
    }))
    .unwrap();

    request.validate().unwrap();
    assert_eq!(
        request.selected_modes,
        vec![
            PlanningMode::Bus,
            PlanningMode::Metro,
            PlanningMode::BikeShare
        ]
    );
    assert_eq!(
        request.allowed_transit_modes(),
        vec![
            via_server::domain::transit::TransitMode::Bus,
            via_server::domain::transit::TransitMode::Metro,
        ]
    );
    assert!(request.allows_bike_share());
}

#[test]
fn planning_request_accepts_modes_in_any_order() {
    let request: PlanningRequest = serde_json::from_value(json!({
        "origin": {
            "coordinate": {"latitude": 25.04776, "longitude": 121.51706},
            "display_text": "Taipei Main Station"
        },
        "destination": {
            "coordinate": {"latitude": 25.03396, "longitude": 121.56447},
            "display_text": "Taipei 101"
        },
        "selected_modes": ["metro", "bus"],
        "departure_setting": {"type": "depart_now"}
    }))
    .unwrap();

    request.validate().unwrap();
    assert_eq!(
        request.allowed_transit_modes(),
        vec![
            via_server::domain::transit::TransitMode::Bus,
            via_server::domain::transit::TransitMode::Metro,
        ]
    );
}

#[test]
fn bike_share_only_is_a_valid_request() {
    let request: PlanningRequest = serde_json::from_value(json!({
        "origin": {
            "coordinate": {"latitude": 25.04776, "longitude": 121.51706},
            "display_text": "Origin"
        },
        "destination": {
            "coordinate": {"latitude": 25.03396, "longitude": 121.56447},
            "display_text": "Destination"
        },
        "selected_modes": ["bike_share"],
        "departure_setting": {"type": "depart_now"}
    }))
    .unwrap();

    request.validate().unwrap();
    assert!(request.allows_bike_share());
}

#[test]
fn planning_request_rejects_duplicate_modes() {
    let duplicate: PlanningRequest = serde_json::from_value(json!({
        "origin": {
            "coordinate": {"latitude": 25.04776, "longitude": 121.51706},
            "display_text": "Origin"
        },
        "destination": {
            "coordinate": {"latitude": 25.03396, "longitude": 121.56447},
            "display_text": "Destination"
        },
        "selected_modes": ["bus", "bus"],
        "departure_setting": {"type": "depart_now"}
    }))
    .unwrap();
    assert_eq!(
        duplicate.validate().unwrap_err().code(),
        "invalid_selected_modes"
    );
}

#[test]
fn tdx_freshness_boundaries_are_exact_and_use_evidence_time() {
    let policy = FreshnessPolicy::tdx_default();
    let evaluated_at = datetime!(2026-08-31 12:00 +08:00);

    let fresh = policy.evaluate(
        LiveStatusTimes::received(datetime!(2026-08-31 11:58 +08:00)),
        evaluated_at,
    );
    let degraded = policy.evaluate(
        LiveStatusTimes::received(datetime!(2026-08-31 11:57:59 +08:00)),
        evaluated_at,
    );
    let unavailable = policy.evaluate(
        LiveStatusTimes::received(datetime!(2026-08-31 11:56:59 +08:00)),
        evaluated_at,
    );

    assert_eq!(fresh, FreshnessState::Fresh);
    assert_eq!(degraded, FreshnessState::Degraded);
    assert_eq!(unavailable, FreshnessState::Unavailable);
}

#[test]
fn validation_error_has_only_a_code_and_json_pointer() {
    let error = RequestValidationError::new("invalid_coordinate", "/origin/coordinate/latitude");
    let serialized = serde_json::to_value(error).unwrap();
    assert_eq!(serialized["code"], "invalid_coordinate");
    assert_eq!(serialized["field"], "/origin/coordinate/latitude");
    assert_eq!(serialized.as_object().unwrap().len(), 2);
}
