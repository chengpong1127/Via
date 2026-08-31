use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{
    live_status::{Freshness, FreshnessReason, FreshnessState},
    snapshot::{
        FeedEvidence, NetworkSnapshot, NormalizedLiveStatus, PublishError, SnapshotError,
        SnapshotStore,
    },
};

#[derive(Debug)]
pub struct FeedRefreshBatch {
    pub freshness: Freshness,
    pub live_status: Vec<NormalizedLiveStatus>,
}

#[async_trait]
pub trait FeedSource: Send + Sync {
    fn capability(&self) -> &str;

    async fn refresh(
        &self,
        refresh_started_at: OffsetDateTime,
    ) -> Result<FeedRefreshBatch, FreshnessReason>;
}

#[derive(Clone)]
pub struct RefreshCoordinator {
    snapshots: Arc<dyn SnapshotStore>,
    sources: Arc<Vec<Arc<dyn FeedSource>>>,
    max_concurrency: usize,
}

impl RefreshCoordinator {
    pub fn new(
        snapshots: Arc<dyn SnapshotStore>,
        sources: Vec<Arc<dyn FeedSource>>,
        max_concurrency: usize,
    ) -> Self {
        Self {
            snapshots,
            sources: Arc::new(sources),
            max_concurrency: max_concurrency.clamp(1, 4),
        }
    }

    pub async fn run(
        &self,
        refresh_key: &str,
        evaluated_at: OffsetDateTime,
    ) -> Result<RefreshOutcome, RefreshError> {
        let Some(lease) = self
            .snapshots
            .acquire_refresh_lease(refresh_key, evaluated_at)
            .await?
        else {
            return Ok(RefreshOutcome::AlreadyHandled);
        };
        let current = self.snapshots.pin_current().await?;
        let policy_version = current.freshness_policy_version.clone();
        let sources = self.sources.as_ref().clone();
        let mut evidence = Vec::with_capacity(sources.len());
        let mut live_status = Vec::new();
        for chunk in sources.chunks(self.max_concurrency) {
            let mut tasks = Vec::with_capacity(chunk.len());
            for source in chunk.iter().cloned() {
                tasks.push(tokio::spawn(refresh_source(
                    source,
                    evaluated_at,
                    policy_version.clone(),
                )));
            }
            for task in tasks {
                let (feed_evidence, mut observations) = task
                    .await
                    .unwrap_or_else(|_| failed_source("unknown", evaluated_at, &policy_version));
                evidence.push(feed_evidence);
                live_status.append(&mut observations);
            }
        }
        let next_version = current.publication_version + 1;
        let published_at = OffsetDateTime::now_utc();
        let snapshot = NetworkSnapshot {
            id: snapshot_id(refresh_key, next_version),
            created_at: published_at,
            live_cutoff: published_at,
            static_network_revision: current.static_network_revision,
            schedule_revision: current.schedule_revision,
            freshness_policy_version: current.freshness_policy_version,
            publication_version: next_version,
            feed_evidence: evidence,
            live_status,
        };
        self.snapshots
            .publish(lease, current.publication_version, snapshot.clone())
            .await?;
        Ok(RefreshOutcome::Published {
            snapshot_id: snapshot.id,
            publication_version: next_version,
        })
    }
}

async fn refresh_source(
    source: Arc<dyn FeedSource>,
    evaluated_at: OffsetDateTime,
    policy_version: String,
) -> (FeedEvidence, Vec<NormalizedLiveStatus>) {
    let capability = source.capability().to_owned();
    match source.refresh(evaluated_at).await {
        Ok(batch) => (
            FeedEvidence {
                capability,
                freshness: batch.freshness,
                failure_reason: None,
            },
            batch.live_status,
        ),
        Err(reason) => failed_source_with_reason(capability, evaluated_at, policy_version, reason),
    }
}

fn failed_source(
    capability: &str,
    evaluated_at: OffsetDateTime,
    policy_version: &str,
) -> (FeedEvidence, Vec<NormalizedLiveStatus>) {
    failed_source_with_reason(
        capability.into(),
        evaluated_at,
        policy_version.into(),
        FreshnessReason::SourceError,
    )
}

fn failed_source_with_reason(
    capability: String,
    evaluated_at: OffsetDateTime,
    policy_version: String,
    reason: FreshnessReason,
) -> (FeedEvidence, Vec<NormalizedLiveStatus>) {
    (
        FeedEvidence {
            capability,
            freshness: Freshness {
                state: FreshnessState::Unavailable,
                reason: Some(reason),
                policy_version,
                evaluated_at,
            },
            failure_reason: Some(reason),
        },
        Vec::new(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RefreshOutcome {
    Published {
        snapshot_id: String,
        publication_version: u64,
    },
    AlreadyHandled,
}

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Publish(#[from] PublishError),
}

fn snapshot_id(refresh_key: &str, publication_version: u64) -> String {
    let mut digest = Sha256::new();
    digest.update(b"via.snapshot.v1\0");
    digest.update(refresh_key.as_bytes());
    digest.update(publication_version.to_be_bytes());
    format!("snapshot_{:x}", digest.finalize())
}
