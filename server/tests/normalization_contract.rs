use time::macros::{date, datetime};
use via_server::normalization::{
    AliasKey, AliasRegistry, ArrivalState, BoardingPointId, EntityId, Provider, ServiceDayPolicy,
    TransitServiceId,
};
use via_server::tdx::normalize_n1;

#[test]
fn reviewed_alias_change_preserves_via_identity() {
    let mut registry = AliasRegistry::default();
    let via_id = EntityId::BoardingPoint(BoardingPointId::new("boarding_opaque"));
    registry
        .register_reviewed(
            AliasKey::new(Provider::Tdx, "boarding_point", "taipei", "old-stop"),
            via_id.clone(),
            datetime!(2026-01-01 00:00 UTC),
        )
        .unwrap();
    registry
        .register_reviewed(
            AliasKey::new(Provider::Tdx, "boarding_point", "taipei", "new-stop"),
            via_id.clone(),
            datetime!(2026-08-31 00:00 UTC),
        )
        .unwrap();

    assert_eq!(
        registry.resolve(
            &AliasKey::new(Provider::Tdx, "boarding_point", "taipei", "new-stop"),
            datetime!(2026-09-01 00:00 UTC)
        ),
        Some(&via_id)
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
