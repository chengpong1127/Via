use std::time::Instant;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SearchDiagnostics {
    pub(super) access_candidate_count: usize,
    pub(super) egress_candidate_count: usize,
    pub(super) initial_need_count: usize,
    pub(super) unique_departure_query_count: usize,
    pub(super) departure_batch_count: usize,
    pub(super) trip_state_count: usize,
    pub(super) scanned_downstream_stop_count: usize,
    pub(super) transfer_expansions: usize,
    pub(super) same_stop_transfer_expansions: usize,
    pub(super) dynamic_labels_accepted: usize,
    pub(super) dynamic_labels_pruned: usize,
    pub(super) reached_labels_accepted: usize,
    pub(super) reached_labels_pruned: usize,
    pub(super) final_candidate_count: usize,
}

impl SearchDiagnostics {
    pub(super) fn report(&self, started_at: Instant) {
        tracing::debug!(
            target: "via::routing",
            access_candidates = self.access_candidate_count,
            egress_candidates = self.egress_candidate_count,
            initial_needs = self.initial_need_count,
            unique_departure_queries = self.unique_departure_query_count,
            departure_batches = self.departure_batch_count,
            trip_states = self.trip_state_count,
            scanned_downstream_stops = self.scanned_downstream_stop_count,
            transfer_expansions = self.transfer_expansions,
            same_stop_transfer_expansions = self.same_stop_transfer_expansions,
            dynamic_labels_accepted = self.dynamic_labels_accepted,
            dynamic_labels_pruned = self.dynamic_labels_pruned,
            reached_labels_accepted = self.reached_labels_accepted,
            reached_labels_pruned = self.reached_labels_pruned,
            final_candidates = self.final_candidate_count,
            search_latency_ms = started_at.elapsed().as_millis() as u64,
            "trip-based route search diagnostics"
        );
    }
}
