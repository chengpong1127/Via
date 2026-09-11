use std::{panic::AssertUnwindSafe, sync::Arc};

use async_trait::async_trait;
use futures_util::FutureExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{
    live_status::{Freshness, FreshnessPolicy, FreshnessReason, FreshnessState, LiveStatusTimes},
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

    fn freshness_policy(&self, version: &str) -> FreshnessPolicy {
        FreshnessPolicy::tdx_one_minute(version)
    }

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
        let mut configured_capabilities = std::collections::HashSet::new();
        for source in self.sources.iter() {
            let capability = source.capability();
            if capability.trim().is_empty() || !configured_capabilities.insert(capability) {
                return Err(RefreshError::InvalidConfiguration);
            }
        }
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
                let capability = source.capability().to_owned();
                let task_capability = capability.clone();
                let task_policy_version = policy_version.clone();
                tasks.push((
                    capability,
                    tokio::spawn(async move {
                        AssertUnwindSafe(refresh_source(
                            source,
                            task_capability.clone(),
                            evaluated_at,
                            task_policy_version,
                        ))
                        .catch_unwind()
                        .await
                    }),
                ));
            }
            for (capability, task) in tasks {
                let (feed_evidence, mut observations, policy) = match task.await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) | Err(_) => {
                        tracing::error!(
                            capability = %capability,
                            "feed source task panicked; retaining capability identity"
                        );
                        let policy = FreshnessPolicy::tdx_one_minute(&policy_version);
                        let (evidence, observations) =
                            failed_source(&capability, evaluated_at, &policy_version);
                        (evidence, observations, policy)
                    }
                };
                let capability = feed_evidence.capability.clone();
                let source_failed = feed_evidence.failure_reason;
                let previous = current
                    .live_status
                    .iter()
                    .filter(|status| capability_matches(&capability, status))
                    .map(|status| {
                        re_evaluate_previous(status, evaluated_at, &policy, source_failed)
                    })
                    .collect::<Vec<_>>();
                live_status.retain(|status| !capability_matches(&capability, status));
                if observations.is_empty() {
                    observations = previous;
                }
                evidence.push(feed_evidence);
                live_status.append(&mut observations);
            }
        }
        live_status = deduplicate_observations(live_status);
        evidence.sort_by(|left, right| left.capability.cmp(&right.capability));
        let next_version = current.publication_version + 1;
        let published_at = evaluated_at;
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

fn capability_matches(capability: &str, status: &NormalizedLiveStatus) -> bool {
    if !status.capability.is_empty() {
        return status.capability == capability;
    }
    match &status.value {
        crate::snapshot::LiveStatusValue::ArrivalEstimate(_) => capability.contains("arrival"),
        crate::snapshot::LiveStatusValue::BikeAvailability(_) => capability.contains("bike"),
        crate::snapshot::LiveStatusValue::ServiceCondition(_) => {
            capability.contains("condition") || capability.contains("alert")
        }
    }
}

fn deduplicate_observations(observations: Vec<NormalizedLiveStatus>) -> Vec<NormalizedLiveStatus> {
    let mut unique = std::collections::HashMap::new();
    for observation in observations {
        let key = observation_key(&observation);
        let replace = unique
            .get(&key)
            .is_none_or(|current: &NormalizedLiveStatus| {
                observation_evidence_time(&observation)
                    .cmp(&observation_evidence_time(current))
                    .then(observation.id.cmp(&current.id))
                    .is_gt()
            });
        if replace {
            unique.insert(key, observation);
        }
    }
    let mut deduplicated = unique.into_values().collect::<Vec<_>>();
    deduplicated.sort_by(|left, right| left.id.cmp(&right.id));
    deduplicated
}

fn observation_key(status: &NormalizedLiveStatus) -> String {
    match &status.value {
        crate::snapshot::LiveStatusValue::ArrivalEstimate(arrival) => format!(
            "{}:arrival:{}:{}",
            status.capability,
            arrival.transit_service_id.as_str(),
            arrival.boarding_point_id.as_str()
        ),
        crate::snapshot::LiveStatusValue::BikeAvailability(availability) => format!(
            "{}:bike:{}",
            status.capability,
            availability.station_id.as_str()
        ),
        crate::snapshot::LiveStatusValue::ServiceCondition(condition) => format!(
            "{}:condition:{:?}:{:?}",
            status.capability, condition.scope_via_ids, condition.state
        ),
    }
}

fn observation_evidence_time(status: &NormalizedLiveStatus) -> OffsetDateTime {
    observation_times(status).evidence_time()
}

fn re_evaluate_previous(
    status: &NormalizedLiveStatus,
    evaluated_at: OffsetDateTime,
    policy: &FreshnessPolicy,
    source_failure: Option<FreshnessReason>,
) -> NormalizedLiveStatus {
    let times = observation_times(status);
    let mut freshness = policy.evaluate(times, evaluated_at, None);
    if let Some(reason) = source_failure {
        freshness.state = FreshnessState::Unavailable;
        freshness.reason = Some(reason);
    }
    let mut digest = Sha256::new();
    digest.update(b"via.live-status.re-evaluation.v1\0");
    digest.update(status.id.as_bytes());
    digest.update(evaluated_at.unix_timestamp_nanos().to_be_bytes());
    if let Some(reason) = source_failure {
        digest.update(format!("{reason:?}").as_bytes());
    }
    NormalizedLiveStatus {
        id: format!("live_{:x}", digest.finalize()),
        capability: status.capability.clone(),
        received_at: status.received_at,
        normalized_at: status.normalized_at,
        freshness,
        value: status.value.clone(),
    }
}

fn observation_times(status: &NormalizedLiveStatus) -> LiveStatusTimes {
    match &status.value {
        crate::snapshot::LiveStatusValue::ArrivalEstimate(arrival) => LiveStatusTimes {
            measured_at: None,
            published_at: Some(arrival.transmitted_at),
            received_at: status.received_at,
            normalized_at: status.normalized_at,
        },
        crate::snapshot::LiveStatusValue::BikeAvailability(availability) => LiveStatusTimes {
            measured_at: availability.source_change_at,
            published_at: availability.provider_published_at,
            received_at: availability.received_at,
            normalized_at: status.normalized_at,
        },
        crate::snapshot::LiveStatusValue::ServiceCondition(_) => {
            LiveStatusTimes::received(status.received_at)
        }
    }
}

async fn refresh_source(
    source: Arc<dyn FeedSource>,
    capability: String,
    evaluated_at: OffsetDateTime,
    policy_version: String,
) -> (FeedEvidence, Vec<NormalizedLiveStatus>, FreshnessPolicy) {
    let policy = source.freshness_policy(&policy_version);
    match source.refresh(evaluated_at).await {
        Ok(batch) => {
            let mut observations = Vec::with_capacity(batch.live_status.len());
            for mut status in batch.live_status {
                if !status.capability.is_empty() && status.capability != capability {
                    tracing::warn!(
                        capability = %capability,
                        observation_capability = %status.capability,
                        "discarded observation returned under the wrong feed capability"
                    );
                    continue;
                }
                status.capability = capability.clone();
                status.freshness = policy.evaluate(observation_times(&status), evaluated_at, None);
                observations.push(status);
            }
            let mut freshness = batch.freshness;
            freshness.policy_version = policy.version.clone();
            freshness.evaluated_at = evaluated_at;
            (
                FeedEvidence {
                    capability,
                    freshness,
                    failure_reason: None,
                },
                observations,
                policy,
            )
        }
        Err(reason) => {
            tracing::warn!(
                capability = %capability,
                reason = ?reason,
                "feed source returned an explicit failure"
            );
            let (evidence, observations) =
                failed_source_with_reason(capability, evaluated_at, policy.version.clone(), reason);
            (evidence, observations, policy)
        }
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
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
    #[error("refresh source capabilities must be unique and non-empty")]
    InvalidConfiguration,
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
