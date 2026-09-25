use via_server::domain::{
    ids::{EffectiveTripId, ServiceCalendarId, TransitPatternId, TransitTripId},
    transit::{ScheduledStopTime, TransitTrip},
    transit_timing::{EffectiveTrip, TimingBasis, TimingReliability},
};

#[test]
fn effective_trip_keeps_an_optional_scheduled_trip_origin() {
    let scheduled = TransitTrip::new(
        TransitTripId::new("scheduled"),
        TransitPatternId::new("service"),
        ServiceCalendarId::new("monday"),
        vec![ScheduledStopTime::new(36_000, 36_000)],
    );
    let effective = EffectiveTrip {
        id: EffectiveTripId::new("effective"),
        pattern_id: scheduled.pattern_id.clone(),
        scheduled_trip_id: Some(scheduled.id.clone()),
        stop_times: Vec::new(),
        timing_basis: TimingBasis::Schedule,
        timing_reliability: TimingReliability::Reliable,
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    };
    let dynamic = EffectiveTrip {
        id: EffectiveTripId::new("dynamic"),
        pattern_id: scheduled.pattern_id,
        scheduled_trip_id: None,
        stop_times: Vec::new(),
        timing_basis: TimingBasis::FrequencyEstimate,
        timing_reliability: TimingReliability::Fallback,
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    };

    assert_eq!(effective.scheduled_trip_id, Some(scheduled.id));
    assert_eq!(effective.id, EffectiveTripId::new("effective"));
    assert!(dynamic.scheduled_trip_id.is_none());
    assert_eq!(dynamic.id, EffectiveTripId::new("dynamic"));
}
