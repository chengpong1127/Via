use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use futures_util::future::join_all;

use crate::domain::{
    transit::{TransitMode, TransitNetwork},
    transit_timing::{
        DepartureError, DepartureQuery, DepartureResolver, EffectiveTrip, TimingReliability,
    },
};

/// Owns the complete departure policy for one transit mode.
///
/// A source may omit realtime entirely. In that case the default resolution
/// path uses its static implementation directly. When realtime is present,
/// fallback is decided independently for every query in a batch.
#[async_trait]
pub trait TransitDepartureSource: Send + Sync {
    fn mode(&self) -> TransitMode;

    fn static_timing_reliability(&self, query: &DepartureQuery) -> TimingReliability;

    async fn static_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError>;

    async fn static_departures_batch(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Vec<Result<Vec<EffectiveTrip>, DepartureError>> {
        join_all(
            queries
                .into_iter()
                .map(|query| async move { self.static_departures(query).await }),
        )
        .await
    }

    /// Returns `None` when this mode has no realtime implementation.
    async fn realtime_departures_batch(
        &self,
        _queries: Vec<DepartureQuery>,
    ) -> Option<Vec<Result<Vec<EffectiveTrip>, DepartureError>>> {
        None
    }

    async fn resolve_batch(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Vec<Result<Vec<EffectiveTrip>, DepartureError>> {
        if queries.is_empty() {
            return Vec::new();
        }

        let realtime = self.realtime_departures_batch(queries.clone()).await;
        let mut results = vec![None; queries.len()];
        let mut fallback_queries = Vec::new();
        let mut fallback_indices = Vec::new();

        for (index, query) in queries.iter().enumerate() {
            match realtime.as_ref().and_then(|batch| batch.get(index)) {
                Some(Ok(departures)) if !departures.is_empty() => {
                    results[index] = Some(Ok(departures.clone()));
                }
                Some(Ok(_)) | Some(Err(_)) | None => {
                    fallback_indices.push(index);
                    fallback_queries.push(query.clone());
                }
            }
        }

        let fallback = self.static_departures_batch(fallback_queries).await;
        for (position, index) in fallback_indices.into_iter().enumerate() {
            let query = &queries[index];
            let result = fallback
                .get(position)
                .cloned()
                .unwrap_or(Err(DepartureError::TemporarilyUnavailable))
                .map(|departures| {
                    departures
                        .into_iter()
                        .map(|mut departure| {
                            departure.timing_reliability = self.static_timing_reliability(query);
                            departure
                        })
                        .collect()
                });
            results[index] = Some(result);
        }

        results
            .into_iter()
            .map(|result| result.unwrap_or(Err(DepartureError::TemporarilyUnavailable)))
            .collect()
    }
}

/// Adapts a mode-owned source to the algorithm-facing departure boundary.
#[derive(Clone)]
pub struct DepartureDispatcher {
    network: Arc<TransitNetwork>,
    sources: HashMap<TransitMode, Arc<dyn TransitDepartureSource>>,
}

impl DepartureDispatcher {
    pub fn new(
        network: Arc<TransitNetwork>,
        sources: impl IntoIterator<Item = Arc<dyn TransitDepartureSource>>,
    ) -> Self {
        let sources = sources
            .into_iter()
            .map(|source| (source.mode(), source))
            .collect();
        Self { network, sources }
    }

    pub fn with_source(mut self, source: Arc<dyn TransitDepartureSource>) -> Self {
        self.sources.insert(source.mode(), source);
        self
    }
}

#[async_trait]
impl DepartureResolver for DepartureDispatcher {
    async fn next_departures(
        &self,
        query: DepartureQuery,
    ) -> Result<Vec<EffectiveTrip>, DepartureError> {
        self.next_departures_batch(vec![query])
            .await
            .pop()
            .unwrap_or(Err(DepartureError::TemporarilyUnavailable))
    }

    async fn next_departures_batch(
        &self,
        queries: Vec<DepartureQuery>,
    ) -> Vec<Result<Vec<EffectiveTrip>, DepartureError>> {
        if queries.is_empty() {
            return Vec::new();
        }

        let mut results = vec![None; queries.len()];
        let mut grouped = HashMap::<TransitMode, Vec<(usize, DepartureQuery)>>::new();
        for (index, query) in queries.into_iter().enumerate() {
            let Some(service) = self.network.service(&query.service_id) else {
                results[index] = Some(Err(DepartureError::InvalidQuery));
                continue;
            };
            grouped
                .entry(service.transit_mode)
                .or_default()
                .push((index, query));
        }

        for (mode, entries) in grouped {
            let Some(source) = self.sources.get(&mode) else {
                for (index, _) in entries {
                    results[index] = Some(Err(DepartureError::InvalidConfiguration));
                }
                continue;
            };
            let indexes = entries.iter().map(|(index, _)| *index).collect::<Vec<_>>();
            let source_queries = entries
                .into_iter()
                .map(|(_, query)| query)
                .collect::<Vec<_>>();
            let source_results = source.resolve_batch(source_queries).await;
            for (position, index) in indexes.into_iter().enumerate() {
                results[index] = Some(
                    source_results
                        .get(position)
                        .cloned()
                        .unwrap_or(Err(DepartureError::TemporarilyUnavailable)),
                );
            }
        }

        results
            .into_iter()
            .map(|result| result.unwrap_or(Err(DepartureError::TemporarilyUnavailable)))
            .collect()
    }
}
