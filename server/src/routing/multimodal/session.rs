use super::*;

pub(crate) struct PlanningSession<'a> {
    pub(super) network: &'a TransitRuntime,
    pub(super) query: PlanningQuery,
    pub(super) retained: static_search::RetainedCorridorSet,
    pub(super) corridors: Vec<EvaluationCorridor>,
    pub(super) snapshots: RequestSnapshots,
    pub(super) walking: WalkingCache,
    pub(super) bike_share: Option<BikeShareSession>,
    pub(super) static_stats: static_search::StaticSearchStats,
}

pub(crate) struct PlanningReport {
    pub outcome: Result<Vec<RealizedJourney>, RoutingError>,
    pub stats: MultimodalPlannerStats,
    pub failures: crate::routing::failures::FailureSummary,
}

impl PlanningSession<'_> {
    #[tracing::instrument(target = "via_profile", name = "Phase 2", skip_all, fields(corridor_count = self.corridors.len()))]
    pub async fn evaluate(self) -> PlanningReport {
        let report = evaluator::CorridorScheduler::new(
            self.network,
            &self.retained,
            &self.snapshots.timing,
            &self.query,
            &self.walking,
            self.bike_share.as_ref(),
        )
        .evaluate(&self.corridors)
        .await;
        let mut failures = report.failures;
        failures.merge(self.snapshots.failures);
        if report.outcome.is_ok() {
            ::metrics::counter!("via_planner_transit_departure_lookups_total")
                .increment(report.stats.transit_departure_lookups as u64);
        }
        PlanningReport {
            outcome: report.outcome,
            stats: MultimodalPlannerStats {
                phase2_evaluation: report.stats,
                phase1: self.static_stats,
            },
            failures,
        }
    }
}
