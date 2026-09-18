use time::macros::{date, datetime};
use via_server::domain::{
    ids::{BoardingPointId, TransitServiceId},
    realtime::{ArrivalState, ServiceDayPolicy},
};
use via_server::providers::{
    tdx::{boarding_point_id, transit_service_id, transit_service_id_for_direction},
    timing::normalize_n1,
};

#[test]
fn static_and_realtime_tdx_records_share_typed_ids() {
    let static_stop = boarding_point_id("TPE-stop-1");
    let realtime_stop = BoardingPointId::new("TPE-stop-1");
    assert_eq!(static_stop, realtime_stop);

    let static_service = transit_service_id("TPE-service-1");
    let realtime_service = TransitServiceId::new("TPE-service-1");
    assert_eq!(static_service, realtime_service);
}

#[test]
fn tdx_authority_codes_and_directions_are_deterministic_namespaces() {
    assert_ne!(
        boarding_point_id("TPE-stop-1"),
        boarding_point_id("KHH-stop-1")
    );
    assert_ne!(
        transit_service_id_for_direction("TPE-service-1", 0),
        transit_service_id_for_direction("TPE-service-1", 1)
    );
}

#[test]
fn n1_nulls_and_statuses_never_collapse_into_zero() {
    let service = TransitServiceId::new("service_opaque");
    let boarding = BoardingPointId::new("boarding_opaque");
    let anchor = datetime!(2026-08-31 12:00 +08:00);

    assert_eq!(
        normalize_n1(service.clone(), boarding.clone(), 0, None, anchor).state,
        ArrivalState::Unknown
    );
    assert_eq!(
        normalize_n1(service.clone(), boarding.clone(), 1, Some(120), anchor).state,
        ArrivalState::NotDeparted
    );
    assert_eq!(
        normalize_n1(service.clone(), boarding.clone(), 2, None, anchor).state,
        ArrivalState::Skipped
    );
    assert_eq!(
        normalize_n1(service, boarding, 0, Some(0), anchor).state,
        ArrivalState::AtBoardingPoint
    );
}

#[test]
fn after_midnight_service_uses_the_previous_service_day() {
    let policy = ServiceDayPolicy::new(time::Time::from_hms(4, 0, 0).unwrap());
    assert_eq!(
        policy.resolve(datetime!(2026-09-01 01:30)),
        date!(2026 - 08 - 31)
    );
    assert_eq!(
        policy.resolve(datetime!(2026-09-01 05:00)),
        date!(2026 - 09 - 01)
    );
}
