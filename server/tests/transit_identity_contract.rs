use via_server::domain::{
    ids::{EffectiveTripId, ScheduledTripId, TransitServiceId},
    transit::{ScheduledStopTime, ScheduledTrip},
    transit_timing::{EffectiveTrip, TimingBasis},
};

#[test]
fn effective_trip_keeps_an_optional_scheduled_trip_origin() {
    let scheduled = ScheduledTrip::weekdays(
        ScheduledTripId::new("scheduled"),
        TransitServiceId::new("service"),
        vec![time::Weekday::Monday],
        vec![ScheduledStopTime::new(36_000, 36_000)],
    );
    let effective = EffectiveTrip {
        id: EffectiveTripId::new("effective"),
        service_id: scheduled.service_id.clone(),
        scheduled_trip_id: Some(scheduled.id.clone()),
        stop_times: Vec::new(),
        timing_basis: TimingBasis::Schedule,
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    };
    let dynamic = EffectiveTrip {
        id: EffectiveTripId::new("dynamic"),
        service_id: scheduled.service_id,
        scheduled_trip_id: None,
        stop_times: Vec::new(),
        timing_basis: TimingBasis::FrequencyEstimate,
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    };

    assert_eq!(effective.scheduled_trip_id, Some(scheduled.id));
    assert_eq!(effective.id, EffectiveTripId::new("effective"));
    assert!(dynamic.scheduled_trip_id.is_none());
    assert_eq!(dynamic.id, EffectiveTripId::new("dynamic"));
}
