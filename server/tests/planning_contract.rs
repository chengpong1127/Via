use serde_json::json;
use time::macros::{date, datetime};
use via_server::{
    api::{
        DepartureSetting, JourneyMode, PlanningRequest, RequestValidationError, normalize_to_taipei,
    },
    domain::realtime::{FreshnessPolicy, FreshnessReason, FreshnessState, LiveStatusTimes},
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
        "selected_modes": ["transit"],
        "locale": "zh-Hant-TW",
        "departure_setting": {"type": "depart_now"}
    }))
    .unwrap();

    request.validate().unwrap();
    assert_eq!(request.selected_modes, vec![JourneyMode::Transit]);
}

#[test]
fn planning_request_rejects_noncanonical_or_sensitive_input_without_echoing_it() {
    let request: PlanningRequest = serde_json::from_value(json!({
        "origin": {
            "coordinate": {"latitude": 25.04776, "longitude": 121.51706},
            "display_text": "Taipei Main Station"
        },
        "destination": {
            "coordinate": {"latitude": 25.03396, "longitude": 121.56447},
            "display_text": "Taipei 101"
        },
        "selected_modes": ["walking", "transit"],
        "locale": "en",
        "departure_setting": {"type": "depart_now"}
    }))
    .unwrap();

    let error = request.validate().unwrap_err();
    assert_eq!(error.field(), "/selected_modes");
    assert_eq!(error.code(), "invalid_selected_modes");
    assert!(!error.to_string().contains("Taipei Main Station"));
}

#[test]
fn walking_is_implicit_and_cannot_be_selected_as_a_filter() {
    let request: PlanningRequest = serde_json::from_value(json!({
        "origin": {
            "coordinate": {"latitude": 25.04776, "longitude": 121.51706},
            "display_text": "Origin"
        },
        "destination": {
            "coordinate": {"latitude": 25.03396, "longitude": 121.56447},
            "display_text": "Destination"
        },
        "selected_modes": ["walking"],
        "locale": "en",
        "departure_setting": {"type": "depart_now"}
    }))
    .unwrap();

    let error = request.validate().unwrap_err();
    assert_eq!(error.code(), "invalid_selected_modes");
    assert_eq!(error.field(), "/selected_modes");
}

#[test]
fn tdx_freshness_boundaries_are_exact_and_use_evidence_time() {
    let policy = FreshnessPolicy::tdx_one_minute("1");
    let evaluated_at = datetime!(2026-08-31 12:00 +08:00);

    let fresh = policy.evaluate(
        LiveStatusTimes::received(datetime!(2026-08-31 11:58 +08:00)),
        evaluated_at,
        None,
    );
    let degraded = policy.evaluate(
        LiveStatusTimes::received(datetime!(2026-08-31 11:57:59 +08:00)),
        evaluated_at,
        None,
    );
    let unavailable = policy.evaluate(
        LiveStatusTimes::received(datetime!(2026-08-31 11:56:59 +08:00)),
        evaluated_at,
        None,
    );

    assert_eq!(fresh.state, FreshnessState::Fresh);
    assert_eq!(degraded.state, FreshnessState::Degraded);
    assert_eq!(degraded.reason, Some(FreshnessReason::Stale));
    assert_eq!(unavailable.state, FreshnessState::Unavailable);
}

#[test]
fn source_failure_never_makes_an_old_observation_fresh() {
    let policy = FreshnessPolicy::tdx_one_minute("1");
    let result = policy.evaluate(
        LiveStatusTimes::received(datetime!(2026-08-31 11:59:30 +08:00)),
        datetime!(2026-08-31 12:00 +08:00),
        Some(FreshnessReason::SourceError),
    );

    assert_eq!(result.state, FreshnessState::Unavailable);
    assert_eq!(result.reason, Some(FreshnessReason::SourceError));
}

#[test]
fn validation_error_has_only_a_code_and_json_pointer() {
    let error = RequestValidationError::new("invalid_coordinate", "/origin/coordinate/latitude");
    let serialized = serde_json::to_value(error).unwrap();
    assert_eq!(serialized["code"], "invalid_coordinate");
    assert_eq!(serialized["field"], "/origin/coordinate/latitude");
    assert_eq!(serialized.as_object().unwrap().len(), 2);
}
