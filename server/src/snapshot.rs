use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use tokio::sync::RwLock;

use crate::live_status::{Freshness, FreshnessReason};
use crate::normalization::{ArrivalEstimate, BikeAvailability, ServiceCondition};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NetworkSnapshot {
    pub id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub live_cutoff: OffsetDateTime,
    pub static_network_revision: String,
    pub schedule_revision: String,
    pub freshness_policy_version: String,
    #[serde(skip_serializing)]
    pub publication_version: u64,
    #[serde(skip_serializing)]
    pub feed_evidence: Vec<FeedEvidence>,
    #[serde(skip_serializing)]
    pub live_status: Vec<NormalizedLiveStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedLiveStatus {
    pub id: String,
    /// Internal source capability used to merge partial refreshes without
    /// conflating two providers that expose the same value family.
    #[serde(default)]
    pub capability: String,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub normalized_at: OffsetDateTime,
    pub freshness: Freshness,
    #[serde(flatten)]
    pub value: LiveStatusValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "family", content = "value", rename_all = "snake_case")]
pub enum LiveStatusValue {
    ArrivalEstimate(ArrivalEstimate),
    BikeAvailability(BikeAvailability),
    ServiceCondition(ServiceCondition),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedEvidence {
    pub capability: String,
    pub freshness: Freshness,
    pub failure_reason: Option<FreshnessReason>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshLease {
    pub(crate) key: String,
    pub(crate) token: u64,
    pub(crate) expires_at: OffsetDateTime,
}

impl RefreshLease {
    pub fn key(&self) -> &str {
        &self.key
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SnapshotError {
    #[error("no coherent Network Snapshot is published")]
    Unavailable,
    #[error("Network Snapshot storage failed")]
    Storage,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PublishError {
    #[error("refresh lease is missing, expired, or replaced")]
    LeaseLost,
    #[error("the current publication version changed")]
    PublicationVersionChanged,
    #[error("the proposed Network Snapshot version is invalid")]
    InvalidPublicationVersion,
    #[error("Network Snapshot publication storage failed")]
    Storage,
}

#[async_trait]
pub trait SnapshotStore: Send + Sync {
    async fn pin_current(&self) -> Result<NetworkSnapshot, SnapshotError>;

    async fn acquire_refresh_lease(
        &self,
        refresh_key: &str,
        now: OffsetDateTime,
    ) -> Result<Option<RefreshLease>, SnapshotError>;

    async fn publish(
        &self,
        lease: RefreshLease,
        expected_publication_version: u64,
        snapshot: NetworkSnapshot,
    ) -> Result<(), PublishError>;
}

#[derive(Clone)]
pub struct InMemorySnapshotStore {
    state: Arc<RwLock<AuthorityState>>,
    cache: Arc<RwLock<Option<NetworkSnapshot>>>,
    lease_duration: Duration,
}

struct AuthorityState {
    current: NetworkSnapshot,
    active_lease: Option<RefreshLease>,
    completed_refresh_keys: HashSet<String>,
    next_lease_token: u64,
}

impl InMemorySnapshotStore {
    pub fn new(initial: NetworkSnapshot) -> Self {
        Self {
            state: Arc::new(RwLock::new(AuthorityState {
                current: initial,
                active_lease: None,
                completed_refresh_keys: HashSet::new(),
                next_lease_token: 1,
            })),
            cache: Arc::new(RwLock::new(None)),
            lease_duration: Duration::seconds(55),
        }
    }

    pub async fn clear_cache(&self) {
        *self.cache.write().await = None;
    }
}

#[async_trait]
impl SnapshotStore for InMemorySnapshotStore {
    async fn pin_current(&self) -> Result<NetworkSnapshot, SnapshotError> {
        if let Some(snapshot) = self.cache.read().await.clone() {
            return Ok(snapshot);
        }
        let snapshot = self.state.read().await.current.clone();
        *self.cache.write().await = Some(snapshot.clone());
        Ok(snapshot)
    }

    async fn acquire_refresh_lease(
        &self,
        refresh_key: &str,
        now: OffsetDateTime,
    ) -> Result<Option<RefreshLease>, SnapshotError> {
        let mut state = self.state.write().await;
        if state.completed_refresh_keys.contains(refresh_key)
            || state
                .active_lease
                .as_ref()
                .is_some_and(|lease| lease.expires_at > now)
        {
            return Ok(None);
        }
        let lease = RefreshLease {
            key: refresh_key.into(),
            token: state.next_lease_token,
            expires_at: now + self.lease_duration,
        };
        state.next_lease_token += 1;
        state.active_lease = Some(lease.clone());
        Ok(Some(lease))
    }

    async fn publish(
        &self,
        lease: RefreshLease,
        expected_publication_version: u64,
        snapshot: NetworkSnapshot,
    ) -> Result<(), PublishError> {
        let mut state = self.state.write().await;
        if state.active_lease.as_ref() != Some(&lease) {
            return Err(PublishError::LeaseLost);
        }
        if state.current.publication_version != expected_publication_version {
            state.active_lease = None;
            return Err(PublishError::PublicationVersionChanged);
        }
        if snapshot.publication_version != expected_publication_version + 1 {
            state.active_lease = None;
            return Err(PublishError::InvalidPublicationVersion);
        }
        state.current = snapshot.clone();
        state.completed_refresh_keys.insert(lease.key);
        state.active_lease = None;
        *self.cache.write().await = Some(snapshot);
        Ok(())
    }
}
