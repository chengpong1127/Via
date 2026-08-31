use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{
    live_status::{Freshness, FreshnessReason, FreshnessState},
    snapshot::{FeedEvidence, NetworkSnapshot, PublishError, SnapshotError, SnapshotStore},
};

#[async_trait]
pub trait FeedSource: Send + Sync {
    fn capability(&self) -> &str;

    async fn refresh(&self, evaluated_at: OffsetDateTime) -> Result<Freshness, FreshnessReason>;
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
                evidence.push(task.await.unwrap_or_else(|_| FeedEvidence {
                    capability: "unknown".into(),
                    freshness: Freshness {
                        state: FreshnessState::Unavailable,
                        reason: Some(FreshnessReason::SourceError),
                        policy_version: policy_version.clone(),
                        evaluated_at,
                    },
                    failure_reason: Some(FreshnessReason::SourceError),
                }));
            }
        }
        let next_version = current.publication_version + 1;
        let snapshot = NetworkSnapshot {
            id: snapshot_id(refresh_key, next_version),
            created_at: evaluated_at,
            live_cutoff: evaluated_at,
            static_network_revision: current.static_network_revision,
            schedule_revision: current.schedule_revision,
            freshness_policy_version: current.freshness_policy_version,
            publication_version: next_version,
            feed_evidence: evidence,
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
) -> FeedEvidence {
    let capability = source.capability().to_owned();
    match source.refresh(evaluated_at).await {
        Ok(freshness) => FeedEvidence {
            capability,
            freshness,
            failure_reason: None,
        },
        Err(reason) => FeedEvidence {
            capability,
            freshness: Freshness {
                state: FreshnessState::Unavailable,
                reason: Some(reason),
                policy_version,
                evaluated_at,
            },
            failure_reason: Some(reason),
        },
    }
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
