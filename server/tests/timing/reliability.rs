use async_trait::async_trait;
use time::macros::datetime;
use via_server::domain::{
    ids::{BoardingPointId, EffectiveTripId, TransitPatternId},
    transit_timing::{
        DepartureError, DepartureQuery, EffectiveStopTime, EffectiveTrip, TimingBasis,
        TimingReliability,
    },
};
use via_server::providers::timing::TransitDepartureSource;

#[derive(Clone)]
struct FixtureSource {
    realtime: Option<Result<Vec<EffectiveTrip>, DepartureError>>,
    static_result: Result<Vec<EffectiveTrip>, DepartureError>,
}

#[async_trait]
impl TransitDepartureSource for FixtureSource {
    fn mode(&self) -> via_server::domain::transit::TransitMode {
        via_server::domain::transit::TransitMode::Bus
    }

    fn static_timing_reliability(&self, _query: &DepartureQuery) -> TimingReliability {
        TimingReliability::Fallback
    }

    async fn static_departures(
        &self,
        _query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        self.static_result.clone()
    }

    async fn realtime_departures_batch(
        &self,
        _queries: Vec<DepartureQuery>,
    ) -> Option<Vec<Result<Vec<EffectiveTrip>, DepartureError>>> {
        self.realtime.clone().map(|result| vec![result])
    }
}

fn query() -> DepartureQuery {
    DepartureQuery::new(
        TransitPatternId::new("service"),
        BoardingPointId::new("stop-a"),
        datetime!(2026-08-31 10:00 UTC),
        1,
    )
}

fn trip(basis: TimingBasis, reliability: TimingReliability) -> EffectiveTrip {
    EffectiveTrip {
        id: EffectiveTripId::new("effective-trip"),
        pattern_id: TransitPatternId::new("service"),
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
        timing_basis: basis,
        timing_reliability: reliability,
        observed_at: None,
        uncertainty_seconds: None,
        evidence: Vec::new(),
    }
}

#[tokio::test]
async fn empty_realtime_falls_back_to_static_timing() {
    let source = FixtureSource {
        realtime: Some(Ok(Vec::new())),
        static_result: Ok(vec![trip(
            TimingBasis::Schedule,
            TimingReliability::Reliable,
        )]),
    };

    let result = source.resolve_batch(vec![query()]).await;

    assert_eq!(
        result[0].as_ref().unwrap()[0].timing_basis,
        TimingBasis::Schedule
    );
    assert_eq!(
        result[0].as_ref().unwrap()[0].timing_reliability,
        TimingReliability::Fallback
    );
}

#[tokio::test]
async fn realtime_errors_and_unusable_realtime_fall_back_per_query() {
    let error_source = FixtureSource {
        realtime: Some(Err(DepartureError::TimedOut)),
        static_result: Ok(vec![trip(
            TimingBasis::FrequencyEstimate,
            TimingReliability::Reliable,
        )]),
    };
    let unusable_source = FixtureSource {
        realtime: Some(Ok(Vec::new())),
        static_result: Ok(vec![trip(
            TimingBasis::Schedule,
            TimingReliability::Reliable,
        )]),
    };

    assert_eq!(
        error_source.resolve_batch(vec![query()]).await[0]
            .as_ref()
            .unwrap()[0]
            .timing_basis,
        TimingBasis::FrequencyEstimate
    );
    assert_eq!(
        unusable_source.resolve_batch(vec![query()]).await[0]
            .as_ref()
            .unwrap()[0]
            .timing_basis,
        TimingBasis::Schedule
    );
}

#[tokio::test]
async fn static_fallback_errors_are_returned() {
    let source = FixtureSource {
        realtime: Some(Err(DepartureError::RateLimited)),
        static_result: Err(DepartureError::InvalidResponse),
    };

    assert_eq!(
        source.resolve_batch(vec![query()]).await[0],
        Err(DepartureError::InvalidResponse)
    );
}

#[tokio::test]
async fn fresh_realtime_keeps_reliable_timing() {
    let source = FixtureSource {
        realtime: Some(Ok(vec![trip(
            TimingBasis::RealtimePrediction,
            TimingReliability::Reliable,
        )])),
        static_result: Ok(Vec::new()),
    };

    let result = source.resolve_batch(vec![query()]).await;

    assert_eq!(
        result[0].as_ref().unwrap()[0].timing_reliability,
        TimingReliability::Reliable
    );
}
