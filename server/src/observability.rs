use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Process-local operational counters. They intentionally contain only service
/// behavior, never Planning Request fields, Journey contents, or coordinates.
#[derive(Clone, Default)]
pub struct OperationalMetrics {
    inner: Arc<MetricsInner>,
}

#[derive(Default)]
struct MetricsInner {
    planning_requests_total: AtomicU64,
    planning_failures_total: AtomicU64,
    planning_empty_total: AtomicU64,
    planning_options_total: AtomicU64,
    transit_candidate_stops_total: AtomicU64,
    transit_reachable_boardings_total: AtomicU64,
    transit_reachable_patterns_total: AtomicU64,
    transit_realtime_stops_requested_total: AtomicU64,
    transit_temporal_departure_lookups_total: AtomicU64,
    transit_realtime_evidence_values_total: AtomicU64,
    candidate_discovery_duration_ms_total: AtomicU64,
    multimodal_search_duration_ms_total: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlannerStageMetrics {
    pub transit_candidate_stops: usize,
    pub transit_reachable_boardings: usize,
    pub transit_reachable_patterns: usize,
    pub transit_realtime_stops_requested: usize,
    pub transit_temporal_departure_lookups: usize,
    pub transit_realtime_evidence_values: usize,
    pub candidate_discovery_duration_ms: u64,
    pub multimodal_search_duration_ms: u64,
}

impl OperationalMetrics {
    pub fn record_planner_stage(&self, stage: PlannerStageMetrics) {
        self.inner
            .transit_candidate_stops_total
            .fetch_add(stage.transit_candidate_stops as u64, Ordering::Relaxed);
        self.inner
            .transit_reachable_boardings_total
            .fetch_add(stage.transit_reachable_boardings as u64, Ordering::Relaxed);
        self.inner
            .transit_reachable_patterns_total
            .fetch_add(stage.transit_reachable_patterns as u64, Ordering::Relaxed);
        self.inner.transit_realtime_stops_requested_total.fetch_add(
            stage.transit_realtime_stops_requested as u64,
            Ordering::Relaxed,
        );
        self.inner
            .transit_temporal_departure_lookups_total
            .fetch_add(
                stage.transit_temporal_departure_lookups as u64,
                Ordering::Relaxed,
            );
        self.inner.transit_realtime_evidence_values_total.fetch_add(
            stage.transit_realtime_evidence_values as u64,
            Ordering::Relaxed,
        );
        self.inner
            .candidate_discovery_duration_ms_total
            .fetch_add(stage.candidate_discovery_duration_ms, Ordering::Relaxed);
        self.inner
            .multimodal_search_duration_ms_total
            .fetch_add(stage.multimodal_search_duration_ms, Ordering::Relaxed);
    }

    pub fn record_planning_success(&self, options: usize) {
        self.inner
            .planning_requests_total
            .fetch_add(1, Ordering::Relaxed);
        if options == 0 {
            self.inner
                .planning_empty_total
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner
                .planning_options_total
                .fetch_add(options as u64, Ordering::Relaxed);
        }
    }

    pub fn record_planning_failure(&self) {
        self.inner
            .planning_requests_total
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .planning_failures_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn prometheus(&self) -> String {
        let mut output = format!(
            "# TYPE via_planning_requests_total counter\nvia_planning_requests_total {}\n\
# TYPE via_planning_failures_total counter\nvia_planning_failures_total {}\n\
# TYPE via_planning_empty_total counter\nvia_planning_empty_total {}\n\
# TYPE via_planning_options_total counter\nvia_planning_options_total {}\n",
            self.inner.planning_requests_total.load(Ordering::Relaxed),
            self.inner.planning_failures_total.load(Ordering::Relaxed),
            self.inner.planning_empty_total.load(Ordering::Relaxed),
            self.inner.planning_options_total.load(Ordering::Relaxed),
        );
        output.push_str(&format!(
            "# TYPE via_planner_transit_candidate_stops_total counter\nvia_planner_transit_candidate_stops_total {}\n# TYPE via_planner_transit_reachable_boardings_total counter\nvia_planner_transit_reachable_boardings_total {}\n# TYPE via_planner_transit_reachable_patterns_total counter\nvia_planner_transit_reachable_patterns_total {}\n# TYPE via_planner_transit_realtime_stops_requested_total counter\nvia_planner_transit_realtime_stops_requested_total {}\n# TYPE via_planner_transit_temporal_departure_lookups_total counter\nvia_planner_transit_temporal_departure_lookups_total {}\n# TYPE via_planner_transit_realtime_evidence_values_total counter\nvia_planner_transit_realtime_evidence_values_total {}\n# TYPE via_planner_candidate_discovery_duration_ms_total counter\nvia_planner_candidate_discovery_duration_ms_total {}\n# TYPE via_planner_multimodal_search_duration_ms_total counter\nvia_planner_multimodal_search_duration_ms_total {}\n",
            self.inner.transit_candidate_stops_total.load(Ordering::Relaxed),
            self.inner.transit_reachable_boardings_total.load(Ordering::Relaxed),
            self.inner.transit_reachable_patterns_total.load(Ordering::Relaxed),
            self.inner.transit_realtime_stops_requested_total.load(Ordering::Relaxed),
            self.inner.transit_temporal_departure_lookups_total.load(Ordering::Relaxed),
            self.inner
                .transit_realtime_evidence_values_total
                .load(Ordering::Relaxed),
            self.inner.candidate_discovery_duration_ms_total.load(Ordering::Relaxed),
            self.inner.multimodal_search_duration_ms_total.load(Ordering::Relaxed),
        ));
        output
    }
}
