use super::{JourneyCandidate, JourneyReconstructor};
use crate::routing::{RealizedJourney, failures::FailureSummary, model::JourneyPlanningContext};
use futures_util::{StreamExt, stream};
use std::future::Future;

pub(in crate::routing) struct ReconstructionSession {
    reconstructor: JourneyReconstructor,
}

pub(in crate::routing) struct ReconstructionReport {
    pub candidates: Vec<JourneyCandidate>,
    pub failures: FailureSummary,
}

impl ReconstructionSession {
    pub(super) fn new(reconstructor: JourneyReconstructor) -> Self {
        Self { reconstructor }
    }

    #[tracing::instrument(target = "via_profile", name = "Reconstruction", skip_all, fields(candidate_count = journeys.len()))]
    pub(in crate::routing) async fn reconstruct_all(
        self,
        journeys: &[RealizedJourney],
        context: &JourneyPlanningContext<'_>,
        concurrency: usize,
    ) -> ReconstructionReport {
        let results = ordered_reconstruction(
            journeys
                .iter()
                .map(|journey| self.reconstructor.reconstruct(journey, context)),
            concurrency,
        )
        .await;
        let mut candidates = Vec::new();
        let mut failures = FailureSummary::default();
        for result in results {
            match result {
                Ok(Some(candidate)) => candidates.push(candidate),
                Ok(None) => {}
                Err(error) => failures.record_planning(&error),
            }
        }
        tracing::debug!(
            target: "via_server::routing",
            reconstruction_attempts = journeys.len(),
            reconstructed_candidates = candidates.len(),
            "bounded Phase 2 candidates reconstructed"
        );
        ReconstructionReport {
            candidates,
            failures,
        }
    }
}

pub(in crate::routing) async fn ordered_reconstruction<F: Future>(
    futures: impl IntoIterator<Item = F>,
    concurrency: usize,
) -> Vec<F::Output> {
    let indexed = futures
        .into_iter()
        .enumerate()
        .map(index_reconstruction::<F>)
        .collect::<Vec<_>>();
    let mut results = stream::iter(indexed)
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    results.sort_unstable_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

async fn index_reconstruction<F: Future>((index, future): (usize, F)) -> (usize, F::Output) {
    (index, future.await)
}

#[cfg(test)]
mod reconstruction_concurrency_tests {
    use super::*;
    use std::sync::Arc;
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
            let task =
                tokio::spawn(
                    async move { ordered_reconstruction(futures.into_iter(), limit).await },
                );
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
        let task =
            tokio::spawn(async move { ordered_reconstruction(futures.into_iter(), 4).await });
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
            let task = tokio::spawn(async move { ordered_reconstruction(futures, limit).await });
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
