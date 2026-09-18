use super::{
    TripBasedLimits, TripBasedRouter,
    diagnostics::SearchDiagnostics,
    dominance::{dynamic_label_dominates, dynamic_label_dominates_or_equals, insert_dynamic_label},
    state::{BoardingNeed, DepartureCacheKey, DynamicLabel, ReachedTripLabel, TripState},
};
use crate::domain::{
    ids::{EffectiveTripId, TransitServiceId},
    transit_timing::{DepartureError, DepartureQuery, EffectiveTrip},
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

impl TripBasedRouter {
    pub(super) async fn resolve_boardings(
        &self,
        needs: Vec<BoardingNeed>,
        limits: &TripBasedLimits,
        cache: &mut HashMap<DepartureCacheKey, Result<Vec<EffectiveTrip>, DepartureError>>,
        reached_index: &mut HashMap<(EffectiveTripId, usize), Vec<ReachedTripLabel>>,
        dynamic_best: &mut HashMap<(TransitServiceId, usize, usize), Vec<DynamicLabel>>,
        diagnostics: &mut SearchDiagnostics,
    ) -> Vec<TripState> {
        let mut query_frontier =
            HashMap::<(TransitServiceId, usize, usize), Vec<(DynamicLabel, DepartureQuery)>>::new();
        for need in &needs {
            for service_id in self.index.services_at(&need.stop_id) {
                if need
                    .excluded_service
                    .as_ref()
                    .is_some_and(|excluded| excluded == service_id)
                {
                    continue;
                }
                let board_index = self
                    .index
                    .stop_index(service_id, &need.stop_id)
                    .unwrap_or_default();
                let dynamic_key = (service_id.clone(), board_index, need.round);
                let label = DynamicLabel {
                    ready_at: need.ready_at,
                    transfer_count: need.path.transfer_count,
                    walking_seconds: need.path.walking_seconds,
                };
                if dynamic_best.get(&dynamic_key).is_some_and(|labels| {
                    labels
                        .iter()
                        .any(|existing| dynamic_label_dominates_or_equals(existing, &label))
                }) {
                    continue;
                }
                let query = DepartureQuery::new(
                    service_id.clone(),
                    need.stop_id.clone(),
                    need.ready_at,
                    limits.max_departures_per_query,
                );
                let labels = query_frontier.entry(dynamic_key).or_default();
                if labels
                    .iter()
                    .any(|(existing, _)| dynamic_label_dominates_or_equals(existing, &label))
                {
                    continue;
                }
                labels.retain(|(existing, _)| !dynamic_label_dominates(&label, existing));
                labels.push((label, query));
            }
        }
        let mut unique_queries = Vec::<DepartureQuery>::new();
        let mut seen_queries = HashSet::<DepartureCacheKey>::new();
        for candidates in query_frontier.into_values() {
            for (_, query) in candidates {
                let key = DepartureCacheKey(query.clone());
                if !cache.contains_key(&key) && seen_queries.insert(key) {
                    unique_queries.push(query);
                }
            }
        }
        unique_queries.sort_by(|left, right| {
            left.service_id
                .as_str()
                .cmp(right.service_id.as_str())
                .then(
                    left.board_stop_id
                        .as_str()
                        .cmp(right.board_stop_id.as_str()),
                )
                .then(left.after.cmp(&right.after))
        });
        diagnostics.unique_departure_query_count += unique_queries.len();
        if !unique_queries.is_empty() {
            diagnostics.departure_batch_count += 1;
            let results = self
                .departure_resolver
                .next_departures_batch(unique_queries.clone())
                .await;
            for (index, query) in unique_queries.into_iter().enumerate() {
                let result = results
                    .get(index)
                    .cloned()
                    .unwrap_or(Err(DepartureError::TemporarilyUnavailable));
                cache.insert(DepartureCacheKey(query), result);
            }
        }

        let mut states = Vec::new();
        for need in needs {
            for service_id in self.index.services_at(&need.stop_id) {
                if need
                    .excluded_service
                    .as_ref()
                    .is_some_and(|excluded| excluded == service_id)
                {
                    continue;
                }
                let board_index = self
                    .index
                    .stop_index(service_id, &need.stop_id)
                    .unwrap_or_default();
                let dynamic_key = (service_id.clone(), board_index, need.round);
                let label = DynamicLabel {
                    ready_at: need.ready_at,
                    transfer_count: need.path.transfer_count,
                    walking_seconds: need.path.walking_seconds,
                };
                let labels = dynamic_best.entry(dynamic_key).or_default();
                let Some(removed) = insert_dynamic_label(labels, label) else {
                    diagnostics.dynamic_labels_pruned += 1;
                    continue;
                };
                diagnostics.dynamic_labels_accepted += 1;
                diagnostics.dynamic_labels_pruned += removed;
                let query = DepartureQuery::new(
                    service_id.clone(),
                    need.stop_id.clone(),
                    need.ready_at,
                    limits.max_departures_per_query,
                );
                let Some(Ok(trips)) = cache.get(&DepartureCacheKey(query)) else {
                    continue;
                };
                for trip in trips {
                    if &trip.service_id != service_id
                        || trip.stop_times.len() <= board_index + 1
                        || trip.stop_times.get(board_index).is_none_or(|stop_time| {
                            stop_time.stop_id != need.stop_id
                                || stop_time.departure_at < need.ready_at
                        })
                    {
                        continue;
                    }
                    let departure_at = trip.stop_times[board_index].departure_at;
                    let reached_key = (trip.id.clone(), need.round);
                    let current_label = ReachedTripLabel {
                        board_index,
                        departure_at,
                        transfer_count: need.path.transfer_count,
                        walking_seconds: need.path.walking_seconds,
                    };
                    if reached_index.get(&reached_key).is_some_and(|labels| {
                        labels.iter().any(|reached| {
                            reached.board_index <= current_label.board_index
                                && reached.departure_at <= current_label.departure_at
                                && reached.transfer_count <= current_label.transfer_count
                                && reached.walking_seconds <= current_label.walking_seconds
                                && (reached.board_index < current_label.board_index
                                    || reached.departure_at < current_label.departure_at
                                    || reached.transfer_count < current_label.transfer_count
                                    || reached.walking_seconds < current_label.walking_seconds)
                        })
                    }) {
                        diagnostics.reached_labels_pruned += 1;
                        continue;
                    }
                    let labels = reached_index.entry(reached_key).or_default();
                    let label_count = labels.len();
                    labels.retain(|reached| {
                        !(current_label.board_index <= reached.board_index
                            && current_label.departure_at <= reached.departure_at
                            && current_label.transfer_count <= reached.transfer_count
                            && current_label.walking_seconds <= reached.walking_seconds
                            && (current_label.board_index < reached.board_index
                                || current_label.departure_at < reached.departure_at
                                || current_label.transfer_count < reached.transfer_count
                                || current_label.walking_seconds < reached.walking_seconds))
                    });
                    diagnostics.reached_labels_pruned += label_count.saturating_sub(labels.len());
                    labels.push(current_label);
                    diagnostics.reached_labels_accepted += 1;
                    let changed = need
                        .previous_service
                        .as_ref()
                        .is_some_and(|previous| previous != service_id);
                    let path = need.path.with_trip_timing(trip, changed);
                    states.push(TripState {
                        trip: Arc::new(trip.clone()),
                        board_index,
                        round: need.round,
                        path: Arc::new(path),
                    });
                }
            }
        }
        states.sort_by(|left, right| {
            left.trip.stop_times[left.board_index]
                .departure_at
                .cmp(&right.trip.stop_times[right.board_index].departure_at)
                .then(left.trip.id.as_str().cmp(right.trip.id.as_str()))
        });
        states
    }
}
