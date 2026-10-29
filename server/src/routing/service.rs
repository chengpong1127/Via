use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::Arc,
    time::Instant,
};

use futures_util::{Stream, StreamExt, stream};
use time::OffsetDateTime;

use crate::{
    api::PlanningRequest,
    domain::{
        bikeshare::BikeShareNetwork,
        journey::{EmptyReason, EmptyResult, OptionsResult, PlanningError, PlanningOutcome},
        ranking::RankingPolicy,
        transit_runtime::TransitRuntime,
        transit_timing::TimingSnapshotLoader,
    },
    providers::BikeAvailabilitySource,
    street::{ProviderFailureClass, StreetRouter, StreetRoutingError},
};

use super::{
    model::{JourneyPlanningContext, JourneyPlanningLimits, PlanningDeadline, PlanningQuery},
    planner::MultimodalPlanner,
    realized::RealizedJourney,
    reconstruct::{
        JourneyCandidate, JourneyReconstructor, apply_ranked_journey, assign_result_local_ids,
        taipei_now, to_street_coordinate,
    },
};

#[derive(Clone)]
pub struct RoutingService {
    planner: MultimodalPlanner,
    reconstructor: JourneyReconstructor,
    ranking: RankingPolicy,
    limits: JourneyPlanningLimits,
    request_timeout: Option<std::time::Duration>,
    request_slots: Arc<tokio::sync::Semaphore>,
}

impl RoutingService {
    pub fn new(
        street_routing: Arc<dyn StreetRouter>,
        transit_runtime: Arc<TransitRuntime>,
        timing: Arc<dyn TimingSnapshotLoader>,
        bike_share_network: Arc<BikeShareNetwork>,
        bike_availability_source: Arc<dyn BikeAvailabilitySource>,
        limits: JourneyPlanningLimits,
        request_timeouts_enabled: bool,
    ) -> Self {
        let planner = MultimodalPlanner::new(
            transit_runtime.clone(),
            timing,
            street_routing.clone(),
            bike_share_network.clone(),
            bike_availability_source,
            limits.bike.clone(),
        );
        let reconstructor =
            JourneyReconstructor::new(street_routing.clone(), transit_runtime, bike_share_network);
        let limits = limits.normalized();
        Self {
            planner,
            reconstructor,
            ranking: RankingPolicy {
                result_limit: limits
                    .ranking
                    .result_limit
                    .min(limits.planner.max_candidates),
                ..limits.ranking.clone()
            },
            request_slots: Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent_requests)),
            request_timeout: request_timeouts_enabled.then_some(limits.request_timeout),
            limits,
        }
    }
}

impl RoutingService {
    #[tracing::instrument(target = "via_profile", name = "Routing Request", skip_all)]
    pub async fn plan(&self, request: PlanningRequest) -> Result<PlanningOutcome, PlanningError> {
        let deadline = self
            .request_timeout
            .map_or_else(PlanningDeadline::disabled, PlanningDeadline::after);
        match deadline
            .run(async {
                let _permit = match self.request_slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::warn!(
                            target: "via_server::routing",
                            "journey planning rejected because routing capacity is exhausted"
                        );
                        return Err(PlanningError::TemporarilyUnavailable);
                    }
                };
                self.plan_multimodal_inner(request, deadline).await
            })
            .await
        {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    target: "via_server::routing",
                    timeout_ms = self.limits.request_timeout.as_millis().min(u128::from(u64::MAX)) as u64,
                    "journey planning timed out"
                );
                Err(PlanningError::TemporarilyUnavailable)
            }
        }
    }

    async fn plan_multimodal_inner(
        &self,
        request: PlanningRequest,
        deadline: PlanningDeadline,
    ) -> Result<PlanningOutcome, PlanningError> {
        let request_started_at = taipei_now();
        let context = JourneyPlanningContext::new(
            &request,
            request_started_at,
            self.limits.planner.max_journey_duration_seconds,
            deadline,
        )?;
        context.ensure_before_deadline()?;

        let attempt = self.plan_candidates(&context).await?;
        self.finalize_plan(
            request_started_at,
            &context,
            attempt.candidates,
            attempt.failures,
        )
    }

    async fn plan_candidates(
        &self,
        context: &JourneyPlanningContext<'_>,
    ) -> Result<PlanningAttempt, PlanningError> {
        let request = context.request;
        let allowed_transit_modes = request.allowed_transit_modes();
        let mut attempt = PlanningAttempt::default();

        let (origin, destination) = (
            to_street_coordinate(request.origin.coordinate).map_err(|_| PlanningError::Internal)?,
            to_street_coordinate(request.destination.coordinate)
                .map_err(|_| PlanningError::Internal)?,
        );
        use tracing::Instrument;
        let query = async {
            Ok::<_, PlanningError>(if allowed_transit_modes.is_empty() {
                PlanningQuery::new(
                    origin,
                    destination,
                    context.deadline,
                    Vec::new(),
                    Vec::new(),
                    context.search_start,
                    self.limits.planner,
                )
                .with_allowed_transit_modes([])
            } else {
                let access_started = Instant::now();
                let access = self
                    .planner
                    .discover_access_points(
                        request.origin.coordinate,
                        context.search_start,
                        context.access_constraints(),
                        &allowed_transit_modes,
                        &self.limits,
                    )
                    .await;
                context.ensure_before_deadline()?;
                let egress = self
                    .planner
                    .discover_egress_points(
                        request.destination.coordinate,
                        context.direct_constraints(),
                        &allowed_transit_modes,
                        &self.limits,
                    )
                    .await;
                context.ensure_before_deadline()?;
                let (access, egress) = match (access, egress) {
                    (Ok(access), Ok(egress)) => (access, egress),
                    (Err(error), _) | (_, Err(error)) => {
                        attempt.failures.record_provider(&error);
                        (Vec::new(), Vec::new())
                    }
                };
                let candidate_stops = access.len() + egress.len();
                let discovery_duration = access_started.elapsed();
                ::metrics::counter!("via_planner_transit_candidate_stops_total")
                    .increment(candidate_stops as u64);
                ::metrics::histogram!("via_planner_candidate_discovery_duration_seconds")
                    .record(discovery_duration.as_secs_f64());
                tracing::debug!(
                    target: "via_server::routing",
                    candidate_discovery_ms = discovery_duration.as_millis() as u64,
                    transit_candidate_stops = candidate_stops,
                    "transit candidate discovery completed"
                );
                tracing::debug!(
                    target: "via_server::routing",
                    access = ?access.iter().map(|point| (
                        point.stop_id.as_str(),
                        self.planner.modes_at_stop(&point.stop_id),
                        point.walking_seconds,
                    )).collect::<Vec<_>>(),
                    egress = ?egress.iter().map(|point| (
                        point.stop_id.as_str(),
                        self.planner.modes_at_stop(&point.stop_id),
                        point.walking_seconds,
                    )).collect::<Vec<_>>(),
                    "discovered transit access and egress"
                );

                PlanningQuery::new(
                    origin,
                    destination,
                    context.deadline,
                    access,
                    egress,
                    context.search_start,
                    self.limits.planner,
                )
                .with_allowed_transit_modes(allowed_transit_modes)
            })
        }
        .instrument(tracing::info_span!(target: "via_profile", "Candidate Discovery"))
        .await?;
        let mut query = query
            .with_bike_share(request.allows_bike_share())
            .with_limits(self.limits.planner);
        query.latest_arrival = context.latest_arrival;

        {
            let search_started = Instant::now();
            let planning_result = async {
                let mut session = self.planner.prepare(query).await?;
                let journeys = session.evaluate().await?;
                context
                    .ensure_before_deadline()
                    .map_err(|_| super::model::RoutingError::DeadlineExceeded)?;
                let realtime_evidence_values: usize = journeys
                    .iter()
                    .map(RealizedJourney::realtime_evidence_count)
                    .sum();
                ::metrics::counter!("via_planner_transit_realtime_evidence_values_total")
                    .increment(realtime_evidence_values as u64);
                let reconstruction_attempts = journeys.len();
                let reconstruction_span = tracing::info_span!(target: "via_profile", "Reconstruction", candidate_count = reconstruction_attempts);
                let journeys = &journeys;
                let reconstructor = self.reconstructor.request_local();
                let reconstructor = &reconstructor;
                let futures = (0..journeys.len()).map(|index| async move {
                    reconstructor.reconstruct(&journeys[index], context).await
                });
                let results = ordered_reconstruction(
                    futures,
                    self.limits.max_concurrent_reconstruction_candidates,
                );
                async {
                    futures_util::pin_mut!(results);
                    while let Some(result) = results.next().await {
                        match result {
                            Ok(Some(candidate)) => attempt.candidates.push(candidate),
                            Ok(None) => {}
                            Err(error) => attempt.failures.record_planning(&error),
                        }
                    }
                }
                .instrument(reconstruction_span)
                .await;
                tracing::debug!(
                    target: "via_server::routing",
                    reconstruction_attempts,
                    reconstructed_candidates = attempt.candidates.len(),
                    "bounded Phase 2 candidates reconstructed"
                );
                attempt.failures.transient |= session.failures().transient;
                attempt.failures.internal |= session.failures().internal;
                Ok::<_, super::model::RoutingError>(())
            }
            .await;
            match planning_result {
                Ok(()) => {
                    let duration = search_started.elapsed();
                    ::metrics::histogram!("via_planner_multimodal_search_duration_seconds")
                        .record(duration.as_secs_f64());
                    tracing::debug!(
                        target: "via_server::routing",
                        multimodal_search_ms = duration.as_millis() as u64,
                        journey_candidates = attempt.candidates.len(),
                        "multimodal planning completed"
                    );
                }
                Err(error) => {
                    match &error {
                        super::model::RoutingError::Timing(timing) => {
                            attempt.failures.transient |= timing.is_transient();
                        }
                        super::model::RoutingError::StaticSearchLimit => {
                            attempt.failures.internal = true;
                        }
                        super::model::RoutingError::DeadlineExceeded => {
                            return Err(PlanningError::TemporarilyUnavailable);
                        }
                        super::model::RoutingError::InvalidQuery => {}
                        super::model::RoutingError::Internal(_) => {
                            attempt.failures.internal = true;
                        }
                    }
                    tracing::error!(error = %error, "transit planning failed");
                }
            }
        }

        Ok(attempt)
    }

    #[tracing::instrument(target = "via_profile", name = "Final Ranking", skip_all, fields(candidate_count = candidates.len()))]
    fn finalize_plan(
        &self,
        generated_at: OffsetDateTime,
        context: &JourneyPlanningContext<'_>,
        candidates: Vec<JourneyCandidate>,
        failures: PlanningFailures,
    ) -> Result<PlanningOutcome, PlanningError> {
        tracing::debug!(
            target: "via_server::routing",
            journey_candidates = candidates.len(),
            "journey candidates ready for ranking"
        );
        if candidates.is_empty() && failures.internal {
            tracing::error!(
                target: "via_server::routing",
                "journey planning failed due to an internal routing error"
            );
            return Err(PlanningError::Internal);
        }
        if candidates.is_empty() && failures.transient {
            tracing::warn!(
                target: "via_server::routing",
                "journey planning has no result because a dependency was temporarily unavailable"
            );
            return Err(PlanningError::TemporarilyUnavailable);
        }
        context.ensure_before_deadline()?;
        let outcome = self.rank_candidates(generated_at, candidates)?;
        context.ensure_before_deadline()?;
        Ok(outcome)
    }

    fn rank_candidates(
        &self,
        generated_at: OffsetDateTime,
        candidates: Vec<JourneyCandidate>,
    ) -> Result<PlanningOutcome, PlanningError> {
        let mut seen_keys = HashSet::new();
        let candidates = candidates
            .into_iter()
            .filter(|candidate| seen_keys.insert(candidate.ranking.stable_key.clone()))
            .collect::<Vec<_>>();
        let candidates_before_ranking = candidates.len();
        let ranked_journeys = self.ranking.rank(
            candidates
                .iter()
                .map(|candidate| candidate.ranking.clone())
                .collect(),
        );
        tracing::debug!(
            target: "via_server::routing",
            candidates_before_ranking,
            candidates_after_ranking = ranked_journeys.len(),
            "journey candidates ranked"
        );
        let mut candidates_by_key = candidates
            .into_iter()
            .map(|candidate| (candidate.ranking.stable_key.clone(), candidate))
            .collect::<HashMap<_, _>>();
        let mut journey_options = Vec::with_capacity(ranked_journeys.len());
        for (index, ranked_journey) in ranked_journeys.into_iter().enumerate() {
            let Some(mut candidate) = candidates_by_key.remove(&ranked_journey.stable_key) else {
                tracing::error!(
                    target: "via_server::routing",
                    "ranking produced a candidate that could not be reconstructed"
                );
                return Err(PlanningError::Internal);
            };
            apply_ranked_journey(&mut candidate.option, &ranked_journey);
            assign_result_local_ids(&mut candidate.option, index);
            journey_options.push(candidate.option);
        }
        if journey_options.is_empty() {
            return Ok(PlanningOutcome::Empty(EmptyResult {
                generated_at,
                empty: EmptyReason {
                    code: "no_feasible_journey_option",
                },
            }));
        }
        Ok(PlanningOutcome::Options(OptionsResult {
            generated_at,
            journey_options,
        }))
    }
}

fn ordered_reconstruction<F: Future>(
    futures: impl Iterator<Item = F>,
    concurrency: usize,
) -> impl Stream<Item = F::Output> {
    stream::once(async move {
        let indexed = futures
            .enumerate()
            .map(index_reconstruction::<F>)
            .collect::<Vec<_>>();
        let mut results = stream::iter(indexed)
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result)
    })
    .flat_map(stream::iter)
}

async fn index_reconstruction<F: Future>((index, future): (usize, F)) -> (usize, F::Output) {
    (index, future.await)
}

#[derive(Default)]
struct PlanningAttempt {
    candidates: Vec<JourneyCandidate>,
    failures: PlanningFailures,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanningFailures {
    pub(crate) transient: bool,
    pub(crate) internal: bool,
}

impl PlanningFailures {
    fn record_provider(&mut self, error: &StreetRoutingError) {
        match error.failure_class() {
            ProviderFailureClass::Unreachable => {}
            ProviderFailureClass::Transient => self.transient = true,
            ProviderFailureClass::Internal => self.internal = true,
        }
    }

    fn record_planning(&mut self, error: &PlanningError) {
        self.transient |= *error == PlanningError::TemporarilyUnavailable;
        self.internal |= *error == PlanningError::Internal;
    }

    #[cfg(test)]
    fn merge(&mut self, other: Self) {
        self.transient |= other.transient;
        self.internal |= other.internal;
    }
}

#[cfg(test)]
mod limits_tests {
    use super::*;
    use crate::routing::model::PlannerLimits;

    #[test]
    fn routing_configuration_preserves_zero_transfers() {
        let limits = JourneyPlanningLimits {
            planner: PlannerLimits {
                max_transfers: 0,
                ..PlannerLimits::default()
            },
            ..JourneyPlanningLimits::default()
        };
        assert_eq!(limits.normalized().planner.max_transfers, 0);
    }
    #[test]
    fn routing_and_query_share_planner_defaults() {
        let query = PlanningQuery::new(
            crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
            crate::street::Coordinate::try_new(25.001, 121.501).unwrap(),
            crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
            Vec::new(),
            Vec::new(),
            OffsetDateTime::now_utc(),
            crate::config::ViaConfig::default().planner_limits(),
        );
        assert_eq!(JourneyPlanningLimits::default().planner, query.limits);
        assert_eq!(query.limits, PlannerLimits::default());
    }

    #[test]
    fn direct_query_limits_preserve_explicit_zero_safety_bounds() {
        let limits = PlannerLimits {
            max_phase1_expansions: 0,
            ..PlannerLimits::default()
        };
        let query = PlanningQuery::new(
            crate::street::Coordinate::try_new(25.0, 121.5).unwrap(),
            crate::street::Coordinate::try_new(25.001, 121.501).unwrap(),
            crate::routing::model::PlanningDeadline::after(std::time::Duration::from_secs(3600)),
            Vec::new(),
            Vec::new(),
            OffsetDateTime::now_utc(),
            crate::config::ViaConfig::default().planner_limits(),
        )
        .with_limits(limits);
        assert_eq!(query.limits.max_phase1_expansions, 0);
        assert_eq!(
            JourneyPlanningLimits {
                planner: limits,
                ..JourneyPlanningLimits::default()
            }
            .normalized()
            .planner
            .max_phase1_expansions,
            1
        );
    }
    #[test]
    fn planning_failures_accumulate_without_treating_unreachable_as_failure() {
        let mut failures = PlanningFailures::default();
        failures.record_provider(&StreetRoutingError::Unreachable);
        assert_eq!(failures, PlanningFailures::default());
        failures.record_provider(&StreetRoutingError::Timeout);
        assert_eq!(
            failures,
            PlanningFailures {
                transient: true,
                internal: false
            }
        );
        let mut internal = PlanningFailures::default();
        internal.record_planning(&PlanningError::Internal);
        failures.merge(internal);
        assert_eq!(
            failures,
            PlanningFailures {
                transient: true,
                internal: true
            }
        );
    }
}

#[cfg(test)]
mod reconstruction_concurrency_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Semaphore, mpsc, oneshot};

    struct Active(Arc<AtomicUsize>);

    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn bounded_futures_overlap_and_keep_input_order() {
        for limit in [1, 2, 4, 8] {
            let active = Arc::new(AtomicUsize::new(0));
            let maximum = Arc::new(AtomicUsize::new(0));
            let (started_tx, mut started_rx) = mpsc::unbounded_channel();
            let (finished_tx, mut finished_rx) = mpsc::unbounded_channel();
            let mut releases = Vec::new();
            let futures = (0..8)
                .map(|id| {
                    let (tx, rx) = oneshot::channel();
                    releases.push(Some(tx));
                    let active = active.clone();
                    let maximum = maximum.clone();
                    let started = started_tx.clone();
                    let finished = finished_tx.clone();
                    async move {
                        let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(count, Ordering::SeqCst);
                        let _active = Active(active);
                        started.send(id).unwrap();
                        rx.await.unwrap();
                        finished.send(id).unwrap();
                        id
                    }
                })
                .collect::<Vec<_>>();
            let task = tokio::spawn(async move {
                ordered_reconstruction(futures.into_iter(), limit)
                    .collect::<Vec<_>>()
                    .await
            });
            for batch in (0..8).step_by(limit) {
                let size = limit.min(8 - batch);
                for _ in 0..size {
                    started_rx.recv().await.unwrap();
                }
                assert_eq!(active.load(Ordering::SeqCst), size);
                for id in (batch..batch + size).rev() {
                    releases[id].take().unwrap().send(()).unwrap();
                    assert_eq!(finished_rx.recv().await.unwrap(), id);
                }
            }
            assert_eq!(task.await.unwrap(), (0..8).collect::<Vec<_>>());
            assert_eq!(maximum.load(Ordering::SeqCst), limit);
            assert_eq!(active.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn dropping_reconstruction_releases_active_futures_and_permits() {
        let active = Arc::new(AtomicUsize::new(0));
        let permits = Arc::new(Semaphore::new(4));
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let futures = (0..4)
            .map(|_| {
                let active = active.clone();
                let permits = permits.clone();
                let started = started_tx.clone();
                async move {
                    let _permit = permits.acquire().await.unwrap();
                    active.fetch_add(1, Ordering::SeqCst);
                    let _active = Active(active);
                    started.send(()).unwrap();
                    std::future::pending::<()>().await;
                }
            })
            .collect::<Vec<_>>();
        let task = tokio::spawn(async move {
            ordered_reconstruction(futures.into_iter(), 4)
                .collect::<Vec<_>>()
                .await
        });
        for _ in 0..4 {
            started_rx.recv().await.unwrap();
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(permits.available_permits(), 4);
    }

    #[tokio::test]
    async fn completion_admits_new_work_and_preserves_results_and_errors() {
        for limit in [2, 4, 8] {
            let (started_tx, mut started_rx) = mpsc::unbounded_channel();
            let (release_tx, release_rx) = oneshot::channel();
            let mut release_rx = Some(release_rx);
            let futures = (0..=limit).map(move |index| {
                let release = if index == 0 { release_rx.take() } else { None };
                let started = started_tx.clone();
                async move {
                    started.send(index).unwrap();
                    if let Some(release) = release {
                        release.await.unwrap();
                    }
                    match index {
                        1 => Ok(None),
                        2 => Err("failure"),
                        _ => Ok(Some(index)),
                    }
                }
            });
            let task = tokio::spawn(async move {
                ordered_reconstruction(futures, limit)
                    .collect::<Vec<_>>()
                    .await
            });
            for index in 0..=limit {
                assert_eq!(started_rx.recv().await.unwrap(), index);
            }
            release_tx.send(()).unwrap();
            let results = task.await.unwrap();
            assert_eq!(results[0], Ok(Some(0)));
            assert_eq!(results[1], Ok(None));
            assert_eq!(results[2], Err("failure"));
            assert_eq!(
                results[limit],
                if limit == 2 {
                    Err("failure")
                } else {
                    Ok(Some(limit))
                }
            );
        }
    }
}

#[cfg(test)]
#[path = "../../tests/support/reconstruction_benchmark.rs"]
mod reconstruction_benchmark;

#[cfg(test)]
#[path = "../../tests/support/reconstruction_representative.rs"]
mod reconstruction_representative;
