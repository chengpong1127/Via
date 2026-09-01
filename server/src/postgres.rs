use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction, DbBackend,
    DbErr, QueryResult, Statement, TransactionTrait,
};
use time::{Duration, OffsetDateTime};
use tokio::sync::RwLock;

use crate::live_status::FreshnessState;
use crate::migration;
use crate::snapshot::{
    FeedEvidence, NetworkSnapshot, NormalizedLiveStatus, PublishError, RefreshLease, SnapshotError,
    SnapshotStore,
};
use crate::tdx::IdentityResolver;

const POSTGRES: DbBackend = DbBackend::Postgres;

fn statement(sql: &str, values: impl IntoIterator<Item = sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(POSTGRES, sql, values)
}

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
        let row = transaction
            .query_one(Statement::from_string(
                POSTGRES,
                "SELECT current_snapshot_id FROM via_refresh_control WHERE singleton = TRUE FOR UPDATE",
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("via_refresh_control".into()))?;
        let current: Option<String> = row.try_get("", "current_snapshot_id")?;
        if current.is_none() {
            insert_snapshot(&transaction, &snapshot).await?;
            transaction
                .execute(statement(
                    "UPDATE via_refresh_control SET current_snapshot_id = $1, publication_version = $2 WHERE singleton = TRUE",
                    [snapshot.id.clone().into(), (snapshot.publication_version as i64).into()],
                ))
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl SnapshotStore for PostgresSnapshotStore {
    async fn pin_current(&self) -> Result<NetworkSnapshot, SnapshotError> {
        let row = self
            .database
            .query_one(Statement::from_string(
                POSTGRES,
                "SELECT current_snapshot_id FROM via_refresh_control WHERE singleton = TRUE",
            ))
            .await
            .map_err(|_| SnapshotError::Storage)?
            .ok_or(SnapshotError::Storage)?;
        let current_snapshot_id: Option<String> = row
            .try_get("", "current_snapshot_id")
            .map_err(|_| SnapshotError::Storage)?;
        let current_snapshot_id = current_snapshot_id.ok_or(SnapshotError::Unavailable)?;
        if let Some(snapshot) = self.cache.read().await.clone() {
            if snapshot.id == current_snapshot_id {
                return Ok(snapshot);
            }
        }
        let row = self
            .database
            .query_one(statement(
                "SELECT s.id, s.created_at, s.live_cutoff, s.static_network_revision, s.schedule_revision, s.freshness_policy_version, s.publication_version, s.feed_evidence, s.selected_observation_ids
                 FROM via_network_snapshots s
                 WHERE s.id = $1",
                [current_snapshot_id.into()],
            ))
            .await
            .map_err(|_| SnapshotError::Storage)?
            .ok_or(SnapshotError::Unavailable)?;
        let publication_version: i64 = get(&row, "publication_version")?;
        let feed_evidence: serde_json::Value = get(&row, "feed_evidence")?;
        let selected_observation_ids: serde_json::Value = get(&row, "selected_observation_ids")?;
        let selected_observation_ids: Vec<String> =
            serde_json::from_value(selected_observation_ids).map_err(|_| SnapshotError::Storage)?;
        let live_status = if selected_observation_ids.is_empty() {
            Vec::new()
        } else {
            let rows = self
                .database
                .query_all(statement(
                    "SELECT observed_payload
                     FROM via_live_status_observations
                     WHERE id = ANY($1)
                     ORDER BY array_position($1::text[], id)",
                    [selected_observation_ids.into()],
                ))
                .await
                .map_err(|_| SnapshotError::Storage)?;
            rows.into_iter()
                .map(|row| {
                    let payload: serde_json::Value = get(&row, "observed_payload")?;
                    serde_json::from_value::<NormalizedLiveStatus>(payload)
                        .map_err(|_| SnapshotError::Storage)
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let snapshot = NetworkSnapshot {
            id: get(&row, "id")?,
            created_at: get(&row, "created_at")?,
            live_cutoff: get(&row, "live_cutoff")?,
            static_network_revision: get(&row, "static_network_revision")?,
            schedule_revision: get(&row, "schedule_revision")?,
            freshness_policy_version: get(&row, "freshness_policy_version")?,
            publication_version: u64::try_from(publication_version)
                .map_err(|_| SnapshotError::Storage)?,
            feed_evidence: serde_json::from_value::<Vec<FeedEvidence>>(feed_evidence)
                .map_err(|_| SnapshotError::Storage)?,
            live_status,
        };
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
        let completed_row = transaction
            .query_one(statement(
                "SELECT EXISTS(SELECT 1 FROM via_refresh_cycles WHERE refresh_key = $1 AND status = 'published') AS completed",
                [refresh_key.into()],
            ))
            .await
            .map_err(|_| SnapshotError::Storage)?
            .ok_or(SnapshotError::Storage)?;
        let completed: bool = get(&completed_row, "completed")?;
        let control = transaction
            .query_one(Statement::from_string(
                POSTGRES,
                "SELECT lease_key, lease_token, lease_expires_at FROM via_refresh_control WHERE singleton = TRUE FOR UPDATE",
            ))
            .await
            .map_err(|_| SnapshotError::Storage)?
            .ok_or(SnapshotError::Storage)?;
        let active_expires_at: Option<OffsetDateTime> = get(&control, "lease_expires_at")?;
        if completed || active_expires_at.is_some_and(|expires_at| expires_at > now) {
            transaction
                .rollback()
                .await
                .map_err(|_| SnapshotError::Storage)?;
            return Ok(None);
        }
        let previous_token: Option<i64> = get(&control, "lease_token")?;
        let token = previous_token.unwrap_or(0).saturating_add(1) as u64;
        let expires_at = now + self.lease_duration;
        transaction
            .execute(statement(
                "UPDATE via_refresh_control SET lease_key = $1, lease_token = $2, lease_expires_at = $3 WHERE singleton = TRUE",
                [refresh_key.into(), (token as i64).into(), expires_at.into()],
            ))
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
        let control = transaction
            .query_one(Statement::from_string(
                POSTGRES,
                "SELECT publication_version, lease_key, lease_token, lease_expires_at FROM via_refresh_control WHERE singleton = TRUE FOR UPDATE",
            ))
            .await
            .map_err(|_| PublishError::Storage)?
            .ok_or(PublishError::Storage)?;
        let publication_version: i64 = publish_get(&control, "publication_version")?;
        let lease_key: Option<String> = publish_get(&control, "lease_key")?;
        let lease_token: Option<i64> = publish_get(&control, "lease_token")?;
        let lease_expires_at: Option<OffsetDateTime> = publish_get(&control, "lease_expires_at")?;
        if lease_key.as_deref() != Some(&lease.key)
            || lease_token != Some(lease.token as i64)
            || lease_expires_at != Some(lease.expires_at)
            || lease.expires_at <= OffsetDateTime::now_utc()
        {
            return Err(PublishError::LeaseLost);
        }
        if publication_version != expected_publication_version as i64 {
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
        transaction
            .execute(statement(
                "UPDATE via_refresh_control
                 SET current_snapshot_id = $1, publication_version = $2, lease_key = NULL, lease_token = NULL, lease_expires_at = NULL
                 WHERE singleton = TRUE",
                [snapshot.id.clone().into(), (snapshot.publication_version as i64).into()],
            ))
            .await
            .map_err(|_| PublishError::Storage)?;
        transaction
            .execute(statement(
                "INSERT INTO via_refresh_cycles (refresh_key, status, snapshot_id, completed_at)
                 VALUES ($1, 'published', $2, NOW())
                 ON CONFLICT (refresh_key) DO UPDATE SET status = 'published', snapshot_id = EXCLUDED.snapshot_id, completed_at = EXCLUDED.completed_at",
                [lease.key.into(), snapshot.id.clone().into()],
            ))
            .await
            .map_err(|_| PublishError::Storage)?;
        for sql in [
            "DELETE FROM via_live_status_observations WHERE expires_at <= NOW()",
            "DELETE FROM via_feed_health WHERE expires_at <= NOW()",
            "DELETE FROM via_refresh_cycles WHERE completed_at < NOW() - INTERVAL '30 days'",
        ] {
            transaction
                .execute(Statement::from_string(POSTGRES, sql))
                .await
                .map_err(|_| PublishError::Storage)?;
        }
        transaction
            .execute(statement(
                "DELETE FROM via_network_snapshots
                 WHERE created_at < NOW() - INTERVAL '7 days'
                   AND id <> $1
                   AND NOT EXISTS (
                       SELECT 1 FROM via_refresh_cycles r WHERE r.snapshot_id = via_network_snapshots.id
                   )",
                [snapshot.id.clone().into()],
            ))
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

fn get<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> Result<T, SnapshotError> {
    row.try_get("", column).map_err(|_| SnapshotError::Storage)
}

fn publish_get<T: sea_orm::TryGetable>(row: &QueryResult, column: &str) -> Result<T, PublishError> {
    row.try_get("", column).map_err(|_| PublishError::Storage)
}

fn json(value: impl serde::Serialize) -> Result<serde_json::Value, DbErr> {
    serde_json::to_value(value).map_err(|error| DbErr::Custom(error.to_string()))
}

async fn insert_snapshot(
    transaction: &DatabaseTransaction,
    snapshot: &NetworkSnapshot,
) -> Result<(), DbErr> {
    let feed_evidence = json(&snapshot.feed_evidence)?;
    let selected_observation_ids = json(
        snapshot
            .live_status
            .iter()
            .map(|live_status| &live_status.id)
            .collect::<Vec<_>>(),
    )?;
    transaction
        .execute(statement(
            "INSERT INTO via_network_snapshots
             (id, created_at, live_cutoff, static_network_revision, schedule_revision, freshness_policy_version, publication_version, feed_evidence, selected_observation_ids)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (id) DO NOTHING",
            [
                snapshot.id.clone().into(),
                snapshot.created_at.into(),
                snapshot.live_cutoff.into(),
                snapshot.static_network_revision.clone().into(),
                snapshot.schedule_revision.clone().into(),
                snapshot.freshness_policy_version.clone().into(),
                (snapshot.publication_version as i64).into(),
                feed_evidence.into(),
                selected_observation_ids.into(),
            ],
        ))
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
        transaction
            .execute(statement(
                "INSERT INTO via_live_status_observations
                 (id, family, observed_payload, measured_at, published_at, received_at, normalized_at, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT (id) DO NOTHING",
                [
                    status.id.clone().into(),
                    family.into(),
                    json(status)?.into(),
                    measured_at.into(),
                    published_at.into(),
                    status.received_at.into(),
                    status.normalized_at.into(),
                    (status.normalized_at + Duration::days(7)).into(),
                ],
            ))
            .await?;
    }
    Ok(())
}

async fn insert_feed_health(
    transaction: &DatabaseTransaction,
    evidence: &[FeedEvidence],
) -> Result<(), DbErr> {
    for feed in evidence {
        transaction
            .execute(statement(
                "INSERT INTO via_feed_health (capability, state, reason, evaluated_at, expires_at)
                 VALUES ($1, $2, $3, $4, $5)",
                [
                    feed.capability.clone().into(),
                    freshness_state_name(feed.freshness.state).into(),
                    feed.failure_reason
                        .map(|reason| format!("{reason:?}").to_lowercase())
                        .into(),
                    feed.freshness.evaluated_at.into(),
                    (feed.freshness.evaluated_at + Duration::days(30)).into(),
                ],
            ))
            .await?;
    }
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
        self.database
            .query_one(statement(
                "SELECT via_id FROM via_provider_aliases
                 WHERE provider = 'tdx' AND entity_kind = $1 AND authority_namespace = $2
                   AND upstream_key = $3 AND valid_from <= NOW()
                   AND (valid_until IS NULL OR NOW() < valid_until)
                 ORDER BY valid_from DESC LIMIT 1",
                [
                    entity_kind.into(),
                    authority_namespace.into(),
                    upstream_key.into(),
                ],
            ))
            .await
            .ok()
            .flatten()
            .and_then(|row| row.try_get("", "via_id").ok())
    }
}
