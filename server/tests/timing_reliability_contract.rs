use std::sync::Arc;

use async_trait::async_trait;
use time::macros::datetime;
use via_server::domain::{
    ids::{BoardingPointId, EffectiveTripId, TransitServiceId},
    realtime::{
        ArrivalEstimate, ArrivalState, Freshness, FreshnessState, NormalizedRealtimeValue,
        RealtimeValue,
    },
    transit_timing::{
        DepartureError, DepartureQuery, DepartureResolver, EffectiveStopTime, EffectiveTrip,
        RealtimeFirstDepartureResolver, TimingBasis,
    },
};

#[derive(Clone)]
struct FixtureResolver {
    result: Result<Vec<EffectiveTrip>, DepartureError>,
}

#[async_trait]
impl DepartureResolver for FixtureResolver {
    async fn next_departures(
        &self,
        _query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        self.result.clone()
    }
}

fn query() -> DepartureQuery {
    DepartureQuery::new(
        TransitServiceId::new("service"),
        BoardingPointId::new("stop-a"),
        datetime!(2026-08-31 10:00 UTC),
        1,
    )
}

fn trip(timing_basis: TimingBasis, evidence: Vec<NormalizedRealtimeValue>) -> EffectiveTrip {
    EffectiveTrip {
        id: EffectiveTripId::new("effective-trip"),
        service_id: TransitServiceId::new("service"),
        scheduled_trip_id: None,
        stop_times: vec![
            EffectiveStopTime {
                stop_id: BoardingPointId::new("stop-a"),
                arrival_at: datetime!(2026-08-31 10:01 UTC),
                departure_at: datetime!(2026-08-31 10:01 UTC),
            },
            EffectiveStopTime {
                stop_id: BoardingPointId::new("stop-b"),
                arrival_at: datetime!(2026-08-31 10:10 UTC),
                departure_at: datetime!(2026-08-31 10:10 UTC),
            },
        ],
        timing_basis,
        observed_at: None,
        uncertainty_seconds: None,
        evidence,
    }
}

fn stale_evidence() -> NormalizedRealtimeValue {
    let now = datetime!(2026-08-31 10:00 UTC);
    NormalizedRealtimeValue {
        id: "stale-arrival".into(),
        received_at: now,
        normalized_at: now,
        provider_published_at: Some(now),
        source_change_at: None,
        freshness: Freshness {
            state: FreshnessState::Degraded,
            reason: None,
            policy_version: "test".into(),
            evaluated_at: now,
        },
        value: RealtimeValue::ArrivalEstimate(ArrivalEstimate {
            transit_service_id: TransitServiceId::new("service"),
            boarding_point_id: BoardingPointId::new("stop-a"),
            predicted_arrival_at: Some(datetime!(2026-08-31 10:01 UTC)),
            state: ArrivalState::Predicted,
            transmitted_at: now,
        }),
    }
}

fn resolver(
    realtime: Result<Vec<EffectiveTrip>, DepartureError>,
    fallback: Result<Vec<EffectiveTrip>, DepartureError>,
) -> RealtimeFirstDepartureResolver {
    RealtimeFirstDepartureResolver::new(
        Arc::new(FixtureResolver { result: realtime }),
        Arc::new(FixtureResolver { result: fallback }),
    )
}

#[tokio::test]
async fn empty_realtime_falls_back_to_static_timing() {
    let resolver = resolver(
        Ok(Vec::new()),
        Ok(vec![trip(TimingBasis::Schedule, Vec::new())]),
    );

    let result = resolver.next_departures(query()).await.unwrap();

    assert_eq!(result[0].timing_basis, TimingBasis::Schedule);
}

#[tokio::test]
async fn realtime_errors_and_stale_evidence_fall_back_to_static_timing() {
    let error_resolver = resolver(
        Err(DepartureError::TimedOut),
        Ok(vec![trip(TimingBasis::FrequencyEstimate, Vec::new())]),
    );
    let stale_resolver = resolver(
        Ok(vec![trip(
            TimingBasis::RealtimePrediction,
            vec![stale_evidence()],
        )]),
        Ok(vec![trip(TimingBasis::Schedule, Vec::new())]),
    );

    assert_eq!(
        error_resolver.next_departures(query()).await.unwrap()[0].timing_basis,
        TimingBasis::FrequencyEstimate
    );
    assert_eq!(
        stale_resolver.next_departures(query()).await.unwrap()[0].timing_basis,
        TimingBasis::Schedule
    );
}

#[tokio::test]
async fn static_fallback_errors_are_returned() {
    let resolver = resolver(
        Err(DepartureError::RateLimited),
        Err(DepartureError::InvalidResponse),
    );

    assert_eq!(
        resolver.next_departures(query()).await,
        Err(DepartureError::InvalidResponse)
    );
}
