use std::{collections::HashMap, sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set},
    ColumnTrait, ConnectOptions, Database, DatabaseConnection, DatabaseTransaction, DbErr,
    EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
    sea_query::OnConflict,
};
use time::{Duration, OffsetDateTime};
use tokio::sync::RwLock;

use crate::entity::{
    feed_health, live_status_observation, network_snapshot, provider_alias, refresh_control,
    refresh_cycle,
};
use crate::live_status::FreshnessState;
use crate::migration;
use crate::snapshot::{
    FeedEvidence, NetworkSnapshot, NormalizedLiveStatus, PublishError, RefreshLease, SnapshotError,
    SnapshotStore,
};
use crate::tdx::IdentityResolver;

#[derive(Clone)]
pub struct PostgresSnapshotStore {
    database: DatabaseConnection,
    cache: Arc<RwLock<Option<NetworkSnapshot>>>,
    lease_duration: Duration,
}

impl PostgresSnapshotStore {
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, DbErr> {
        let mut options = ConnectOptions::new(database_url);
        options
            .max_connections(max_connections.max(1))
            .acquire_timeout(StdDuration::from_secs(5));
        let database = Database::connect(options).await?;
        migration::migrate(&database).await?;
        Ok(Self {
            database,
            cache: Arc::new(RwLock::new(None)),
            lease_duration: Duration::seconds(55),
        })
    }

    pub async fn ensure_initial(&self, snapshot: NetworkSnapshot) -> Result<(), DbErr> {
        let transaction = self.database.begin().await?;
        let control = refresh_control::Entity::find_by_id(true)
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("via_refresh_control".into()))?;
        if control.current_snapshot_id.is_none() {
            insert_snapshot(&transaction, &snapshot).await?;
            let mut active = control.into_active_model();
            active.current_snapshot_id = Set(Some(snapshot.id.clone()));
            active.publication_version = Set(snapshot.publication_version as i64);
            active.update(&transaction).await?;
        }
        transaction.commit().await
    }

    /// Atomically install reviewed provider aliases for a static artifact.
    /// Existing rows remain immutable versions; a new `valid_from` creates a
    /// new mapping rather than mutating the Via identity in place.
    pub async fn upsert_provider_aliases(
        &self,
        aliases: &[ProviderAliasRecord],
    ) -> Result<(), DbErr> {
        let transaction = self.database.begin().await?;
        for alias in aliases {
            provider_alias::Entity::insert(provider_alias::ActiveModel {
                provider: Set(alias.provider.clone()),
                entity_kind: Set(alias.entity_kind.clone()),
                authority_namespace: Set(alias.authority_namespace.clone()),
                upstream_key: Set(alias.upstream_key.clone()),
                via_id: Set(alias.via_id.clone()),
                valid_from: Set(alias.valid_from),
                valid_until: Set(alias.valid_until),
            })
            .on_conflict(
                OnConflict::columns([
                    provider_alias::Column::Provider,
                    provider_alias::Column::EntityKind,
                    provider_alias::Column::AuthorityNamespace,
                    provider_alias::Column::UpstreamKey,
                    provider_alias::Column::ValidFrom,
                ])
                .update_columns([
                    provider_alias::Column::ViaId,
                    provider_alias::Column::ValidUntil,
                ])
                .to_owned(),
            )
            .exec_without_returning(&transaction)
            .await?;
        }
        transaction.commit().await
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderAliasRecord {
    pub provider: String,
    pub entity_kind: String,
    pub authority_namespace: String,
    pub upstream_key: String,
    pub via_id: String,
    pub valid_from: OffsetDateTime,
    pub valid_until: Option<OffsetDateTime>,
}

#[async_trait]
impl SnapshotStore for PostgresSnapshotStore {
    async fn pin_current(&self) -> Result<NetworkSnapshot, SnapshotError> {
        let control = refresh_control::Entity::find_by_id(true)
            .one(&self.database)
            .await
            .map_err(|_| SnapshotError::Storage)?
            .ok_or(SnapshotError::Storage)?;
        let current_snapshot_id = control
            .current_snapshot_id
            .ok_or(SnapshotError::Unavailable)?;
        if let Some(snapshot) = self.cache.read().await.clone() {
            if snapshot.id == current_snapshot_id {
                return Ok(snapshot);
            }
        }
        let model = network_snapshot::Entity::find_by_id(current_snapshot_id)
            .one(&self.database)
            .await
            .map_err(|_| SnapshotError::Storage)?
            .ok_or(SnapshotError::Unavailable)?;
        let selected_observation_ids: Vec<String> =
            serde_json::from_value(model.selected_observation_ids.clone())
                .map_err(|_| SnapshotError::Storage)?;
        let live_status = load_live_status(&self.database, &selected_observation_ids).await?;
        let snapshot = snapshot_from_model(model, live_status)?;
        *self.cache.write().await = Some(snapshot.clone());
        Ok(snapshot)
    }

    async fn acquire_refresh_lease(
        &self,
        refresh_key: &str,
        now: OffsetDateTime,
    ) -> Result<Option<RefreshLease>, SnapshotError> {
        let transaction = self
            .database
            .begin()
            .await
            .map_err(|_| SnapshotError::Storage)?;
        let completed = refresh_cycle::Entity::find_by_id(refresh_key)
            .filter(refresh_cycle::Column::Status.eq("published"))
            .one(&transaction)
            .await
            .map_err(|_| SnapshotError::Storage)?
            .is_some();
        let control = refresh_control::Entity::find_by_id(true)
            .lock_exclusive()
            .one(&transaction)
            .await
            .map_err(|_| SnapshotError::Storage)?
            .ok_or(SnapshotError::Storage)?;
        if completed
            || control
                .lease_expires_at
                .is_some_and(|expires_at| expires_at > now)
        {
            transaction
                .rollback()
                .await
                .map_err(|_| SnapshotError::Storage)?;
            return Ok(None);
        }
        let token = control.lease_token.unwrap_or(0).saturating_add(1) as u64;
        let expires_at = now + self.lease_duration;
        let mut active = control.into_active_model();
        active.lease_key = Set(Some(refresh_key.into()));
        active.lease_token = Set(Some(token as i64));
        active.lease_expires_at = Set(Some(expires_at));
        active
            .update(&transaction)
            .await
            .map_err(|_| SnapshotError::Storage)?;
        transaction
            .commit()
            .await
            .map_err(|_| SnapshotError::Storage)?;
        Ok(Some(RefreshLease {
            key: refresh_key.into(),
            token,
            expires_at,
        }))
    }

    async fn publish(
        &self,
        lease: RefreshLease,
        expected_publication_version: u64,
        snapshot: NetworkSnapshot,
    ) -> Result<(), PublishError> {
        let transaction = self
            .database
            .begin()
            .await
            .map_err(|_| PublishError::Storage)?;
        let control = refresh_control::Entity::find_by_id(true)
            .lock_exclusive()
            .one(&transaction)
            .await
            .map_err(|_| PublishError::Storage)?
            .ok_or(PublishError::Storage)?;
        if control.lease_key.as_deref() != Some(&lease.key)
            || control.lease_token != Some(lease.token as i64)
            || control.lease_expires_at != Some(lease.expires_at)
            || lease.expires_at <= OffsetDateTime::now_utc()
        {
            return Err(PublishError::LeaseLost);
        }
        if control.publication_version != expected_publication_version as i64 {
            return Err(PublishError::PublicationVersionChanged);
        }
        if snapshot.publication_version != expected_publication_version + 1 {
            return Err(PublishError::InvalidPublicationVersion);
        }
        insert_live_status(&transaction, &snapshot.live_status)
            .await
            .map_err(|_| PublishError::Storage)?;
        insert_feed_health(&transaction, &snapshot.feed_evidence)
            .await
            .map_err(|_| PublishError::Storage)?;
        insert_snapshot(&transaction, &snapshot)
            .await
            .map_err(|_| PublishError::Storage)?;

        let mut active = control.into_active_model();
        active.current_snapshot_id = Set(Some(snapshot.id.clone()));
        active.publication_version = Set(snapshot.publication_version as i64);
        active.lease_key = Set(None);
        active.lease_token = Set(None);
        active.lease_expires_at = Set(None);
        active
            .update(&transaction)
            .await
            .map_err(|_| PublishError::Storage)?;

        refresh_cycle::Entity::insert(refresh_cycle::ActiveModel {
            refresh_key: Set(lease.key),
            status: Set("published".into()),
            snapshot_id: Set(Some(snapshot.id.clone())),
            completed_at: Set(OffsetDateTime::now_utc()),
        })
        .on_conflict(
            OnConflict::column(refresh_cycle::Column::RefreshKey)
                .update_columns([
                    refresh_cycle::Column::Status,
                    refresh_cycle::Column::SnapshotId,
                    refresh_cycle::Column::CompletedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(&transaction)
        .await
        .map_err(|_| PublishError::Storage)?;

        cleanup(&transaction, &snapshot.id)
            .await
            .map_err(|_| PublishError::Storage)?;
        transaction
            .commit()
            .await
            .map_err(|_| PublishError::Storage)?;
        *self.cache.write().await = Some(snapshot);
        Ok(())
    }
}

async fn load_live_status(
    database: &DatabaseConnection,
    selected_ids: &[String],
) -> Result<Vec<NormalizedLiveStatus>, SnapshotError> {
    if selected_ids.is_empty() {
        return Ok(Vec::new());
    }
    let models = live_status_observation::Entity::find()
        .filter(live_status_observation::Column::Id.is_in(selected_ids.iter().cloned()))
        .all(database)
        .await
        .map_err(|_| SnapshotError::Storage)?;
    let mut by_id = models
        .into_iter()
        .map(|model| (model.id, model.observed_payload))
        .collect::<HashMap<_, _>>();
    selected_ids
        .iter()
        .map(|id| {
            let payload = by_id.remove(id).ok_or(SnapshotError::Storage)?;
            serde_json::from_value(payload).map_err(|_| SnapshotError::Storage)
        })
        .collect()
}

fn snapshot_from_model(
    model: network_snapshot::Model,
    live_status: Vec<NormalizedLiveStatus>,
) -> Result<NetworkSnapshot, SnapshotError> {
    Ok(NetworkSnapshot {
        id: model.id,
        created_at: model.created_at,
        live_cutoff: model.live_cutoff,
        static_network_revision: model.static_network_revision,
        schedule_revision: model.schedule_revision,
        freshness_policy_version: model.freshness_policy_version,
        publication_version: u64::try_from(model.publication_version)
            .map_err(|_| SnapshotError::Storage)?,
        feed_evidence: serde_json::from_value(model.feed_evidence)
            .map_err(|_| SnapshotError::Storage)?,
        live_status,
    })
}

fn json(value: impl serde::Serialize) -> Result<serde_json::Value, DbErr> {
    serde_json::to_value(value).map_err(|error| DbErr::Custom(error.to_string()))
}

async fn insert_snapshot(
    transaction: &DatabaseTransaction,
    snapshot: &NetworkSnapshot,
) -> Result<(), DbErr> {
    network_snapshot::Entity::insert(network_snapshot::ActiveModel {
        id: Set(snapshot.id.clone()),
        created_at: Set(snapshot.created_at),
        live_cutoff: Set(snapshot.live_cutoff),
        static_network_revision: Set(snapshot.static_network_revision.clone()),
        schedule_revision: Set(snapshot.schedule_revision.clone()),
        freshness_policy_version: Set(snapshot.freshness_policy_version.clone()),
        publication_version: Set(snapshot.publication_version as i64),
        feed_evidence: Set(json(&snapshot.feed_evidence)?),
        selected_observation_ids: Set(json(
            snapshot
                .live_status
                .iter()
                .map(|status| &status.id)
                .collect::<Vec<_>>(),
        )?),
    })
    .on_conflict(
        OnConflict::column(network_snapshot::Column::Id)
            .do_nothing()
            .to_owned(),
    )
    .exec_without_returning(transaction)
    .await?;
    Ok(())
}

async fn insert_live_status(
    transaction: &DatabaseTransaction,
    live_status: &[NormalizedLiveStatus],
) -> Result<(), DbErr> {
    for status in live_status {
        let (family, measured_at, published_at) = match &status.value {
            crate::snapshot::LiveStatusValue::ArrivalEstimate(arrival) => {
                ("arrival_estimate", None, Some(arrival.transmitted_at))
            }
            crate::snapshot::LiveStatusValue::BikeAvailability(availability) => (
                "bike_availability",
                availability.source_change_at,
                availability.provider_published_at,
            ),
            crate::snapshot::LiveStatusValue::ServiceCondition(_) => {
                ("service_condition", None, None)
            }
        };
        live_status_observation::Entity::insert(live_status_observation::ActiveModel {
            id: Set(status.id.clone()),
            family: Set(family.into()),
            observed_payload: Set(json(status)?),
            measured_at: Set(measured_at),
            published_at: Set(published_at),
            received_at: Set(status.received_at),
            normalized_at: Set(status.normalized_at),
            expires_at: Set(status.normalized_at + Duration::days(7)),
        })
        .on_conflict(
            OnConflict::column(live_status_observation::Column::Id)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(transaction)
        .await?;
    }
    Ok(())
}

async fn insert_feed_health(
    transaction: &DatabaseTransaction,
    evidence: &[FeedEvidence],
) -> Result<(), DbErr> {
    for feed in evidence {
        feed_health::ActiveModel {
            id: NotSet,
            capability: Set(feed.capability.clone()),
            state: Set(freshness_state_name(feed.freshness.state).into()),
            reason: Set(feed
                .failure_reason
                .map(|reason| format!("{reason:?}").to_lowercase())),
            evaluated_at: Set(feed.freshness.evaluated_at),
            expires_at: Set(feed.freshness.evaluated_at + Duration::days(30)),
        }
        .insert(transaction)
        .await?;
    }
    Ok(())
}

async fn cleanup(
    transaction: &DatabaseTransaction,
    current_snapshot_id: &str,
) -> Result<(), DbErr> {
    let now = OffsetDateTime::now_utc();
    // Snapshot rows retain observation IDs explicitly. Gather them before any
    // retention work so cleanup can never invalidate a pinned historical view.
    let mut protected_observation_ids = std::collections::HashSet::new();
    for snapshot in network_snapshot::Entity::find().all(transaction).await? {
        let ids = serde_json::from_value::<Vec<String>>(snapshot.selected_observation_ids)
            .map_err(|error| DbErr::Custom(format!("invalid selected observation IDs: {error}")))?;
        protected_observation_ids.extend(ids);
    }
    let mut observation_deletion = live_status_observation::Entity::delete_many()
        .filter(live_status_observation::Column::ExpiresAt.lte(now));
    if !protected_observation_ids.is_empty() {
        observation_deletion = observation_deletion.filter(
            live_status_observation::Column::Id
                .is_not_in(protected_observation_ids.into_iter().collect::<Vec<_>>()),
        );
    }
    observation_deletion.exec(transaction).await?;
    feed_health::Entity::delete_many()
        .filter(feed_health::Column::ExpiresAt.lte(now))
        .exec(transaction)
        .await?;
    refresh_cycle::Entity::delete_many()
        .filter(refresh_cycle::Column::CompletedAt.lt(now - Duration::days(30)))
        .exec(transaction)
        .await?;
    let retained_snapshot_ids = refresh_cycle::Entity::find()
        .all(transaction)
        .await?
        .into_iter()
        .filter_map(|cycle| cycle.snapshot_id)
        .collect::<Vec<_>>();
    let mut deletion = network_snapshot::Entity::delete_many()
        .filter(network_snapshot::Column::CreatedAt.lt(now - Duration::days(7)))
        .filter(network_snapshot::Column::Id.ne(current_snapshot_id));
    if !retained_snapshot_ids.is_empty() {
        deletion = deletion.filter(network_snapshot::Column::Id.is_not_in(retained_snapshot_ids));
    }
    deletion.exec(transaction).await?;
    Ok(())
}

fn freshness_state_name(state: FreshnessState) -> &'static str {
    match state {
        FreshnessState::Fresh => "fresh",
        FreshnessState::Degraded => "degraded",
        FreshnessState::Unavailable => "unavailable",
        FreshnessState::NotApplicable => "not_applicable",
    }
}

#[async_trait]
impl IdentityResolver for PostgresSnapshotStore {
    async fn resolve(
        &self,
        entity_kind: &str,
        authority_namespace: &str,
        upstream_key: &str,
    ) -> Option<String> {
        let now = OffsetDateTime::now_utc();
        provider_alias::Entity::find()
            .filter(provider_alias::Column::Provider.eq("tdx"))
            .filter(provider_alias::Column::EntityKind.eq(entity_kind))
            .filter(provider_alias::Column::AuthorityNamespace.eq(authority_namespace))
            .filter(provider_alias::Column::UpstreamKey.eq(upstream_key))
            .filter(provider_alias::Column::ValidFrom.lte(now))
            .filter(
                provider_alias::Column::ValidUntil
                    .is_null()
                    .or(provider_alias::Column::ValidUntil.gt(now)),
            )
            .order_by_desc(provider_alias::Column::ValidFrom)
            .one(&self.database)
            .await
            .ok()
            .flatten()
            .map(|alias| alias.via_id)
    }
}
