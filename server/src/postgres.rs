use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use time::{Duration, OffsetDateTime};
use tokio::sync::RwLock;

use crate::live_status::FreshnessState;
use crate::snapshot::{
    FeedEvidence, NetworkSnapshot, NormalizedLiveStatus, PublishError, RefreshLease, SnapshotError,
    SnapshotStore,
};
use crate::tdx::IdentityResolver;

#[derive(Clone)]
pub struct PostgresSnapshotStore {
    pool: PgPool,
    cache: Arc<RwLock<Option<NetworkSnapshot>>>,
    lease_duration: Duration,
}

impl PostgresSnapshotStore {
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections.max(1))
            .acquire_timeout(StdDuration::from_secs(5))
            .connect(database_url)
            .await?;
        sqlx::migrate!().run(&pool).await?;
        Ok(Self {
            pool,
            cache: Arc::new(RwLock::new(None)),
            lease_duration: Duration::seconds(55),
        })
    }

    pub async fn ensure_initial(&self, snapshot: NetworkSnapshot) -> Result<(), sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        let current: Option<String> = sqlx::query_scalar(
            "SELECT current_snapshot_id FROM via_refresh_control WHERE singleton = TRUE FOR UPDATE",
        )
        .fetch_one(&mut *transaction)
        .await?;
        if current.is_none() {
            insert_snapshot(&mut transaction, &snapshot).await?;
            sqlx::query(
                "UPDATE via_refresh_control SET current_snapshot_id = $1, publication_version = $2 WHERE singleton = TRUE",
            )
            .bind(&snapshot.id)
            .bind(snapshot.publication_version as i64)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl SnapshotStore for PostgresSnapshotStore {
    async fn pin_current(&self) -> Result<NetworkSnapshot, SnapshotError> {
        let current_snapshot_id: Option<String> = sqlx::query_scalar(
            "SELECT current_snapshot_id FROM via_refresh_control WHERE singleton = TRUE",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| SnapshotError::Storage)?;
        let current_snapshot_id = current_snapshot_id.ok_or(SnapshotError::Unavailable)?;
        if let Some(snapshot) = self.cache.read().await.clone() {
            if snapshot.id == current_snapshot_id {
                return Ok(snapshot);
            }
        }
        let row = sqlx::query(
            "SELECT s.id, s.created_at, s.live_cutoff, s.static_network_revision, s.schedule_revision, s.freshness_policy_version, s.publication_version, s.feed_evidence, s.selected_observation_ids
             FROM via_network_snapshots s
             WHERE s.id = $1",
        )
        .bind(current_snapshot_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| SnapshotError::Storage)?
        .ok_or(SnapshotError::Unavailable)?;
        let publication_version: i64 = row
            .try_get("publication_version")
            .map_err(|_| SnapshotError::Storage)?;
        let feed_evidence: serde_json::Value = row
            .try_get("feed_evidence")
            .map_err(|_| SnapshotError::Storage)?;
        let selected_observation_ids: serde_json::Value = row
            .try_get("selected_observation_ids")
            .map_err(|_| SnapshotError::Storage)?;
        let selected_observation_ids: Vec<String> =
            serde_json::from_value(selected_observation_ids).map_err(|_| SnapshotError::Storage)?;
        let live_status = if selected_observation_ids.is_empty() {
            Vec::new()
        } else {
            let rows = sqlx::query(
                "SELECT observed_payload
                 FROM via_live_status_observations
                 WHERE id = ANY($1)
                 ORDER BY array_position($1::text[], id)",
            )
            .bind(&selected_observation_ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| SnapshotError::Storage)?;
            rows.into_iter()
                .map(|row| {
                    let payload: serde_json::Value = row
                        .try_get("observed_payload")
                        .map_err(|_| SnapshotError::Storage)?;
                    serde_json::from_value::<NormalizedLiveStatus>(payload)
                        .map_err(|_| SnapshotError::Storage)
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let snapshot = NetworkSnapshot {
            id: row.try_get("id").map_err(|_| SnapshotError::Storage)?,
            created_at: row
                .try_get("created_at")
                .map_err(|_| SnapshotError::Storage)?,
            live_cutoff: row
                .try_get("live_cutoff")
                .map_err(|_| SnapshotError::Storage)?,
            static_network_revision: row
                .try_get("static_network_revision")
                .map_err(|_| SnapshotError::Storage)?,
            schedule_revision: row
                .try_get("schedule_revision")
                .map_err(|_| SnapshotError::Storage)?,
            freshness_policy_version: row
                .try_get("freshness_policy_version")
                .map_err(|_| SnapshotError::Storage)?,
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
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SnapshotError::Storage)?;
        let completed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM via_refresh_cycles WHERE refresh_key = $1 AND status = 'published')",
        )
        .bind(refresh_key)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| SnapshotError::Storage)?;
        let control = sqlx::query(
            "SELECT lease_key, lease_token, lease_expires_at FROM via_refresh_control WHERE singleton = TRUE FOR UPDATE",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| SnapshotError::Storage)?;
        let active_expires_at: Option<OffsetDateTime> = control
            .try_get("lease_expires_at")
            .map_err(|_| SnapshotError::Storage)?;
        if completed || active_expires_at.is_some_and(|expires_at| expires_at > now) {
            transaction
                .rollback()
                .await
                .map_err(|_| SnapshotError::Storage)?;
            return Ok(None);
        }
        let previous_token: Option<i64> = control
            .try_get("lease_token")
            .map_err(|_| SnapshotError::Storage)?;
        let token = previous_token.unwrap_or(0).saturating_add(1) as u64;
        let expires_at = now + self.lease_duration;
        sqlx::query(
            "UPDATE via_refresh_control SET lease_key = $1, lease_token = $2, lease_expires_at = $3 WHERE singleton = TRUE",
        )
        .bind(refresh_key)
        .bind(token as i64)
        .bind(expires_at)
        .execute(&mut *transaction)
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
        let mut transaction = self.pool.begin().await.map_err(|_| PublishError::Storage)?;
        let control = sqlx::query(
            "SELECT publication_version, lease_key, lease_token, lease_expires_at FROM via_refresh_control WHERE singleton = TRUE FOR UPDATE",
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| PublishError::Storage)?;
        let publication_version: i64 = control
            .try_get("publication_version")
            .map_err(|_| PublishError::Storage)?;
        let lease_key: Option<String> = control
            .try_get("lease_key")
            .map_err(|_| PublishError::Storage)?;
        let lease_token: Option<i64> = control
            .try_get("lease_token")
            .map_err(|_| PublishError::Storage)?;
        let lease_expires_at: Option<OffsetDateTime> = control
            .try_get("lease_expires_at")
            .map_err(|_| PublishError::Storage)?;
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
        insert_live_status(&mut transaction, &snapshot.live_status)
            .await
            .map_err(|_| PublishError::Storage)?;
        insert_feed_health(&mut transaction, &snapshot.feed_evidence)
            .await
            .map_err(|_| PublishError::Storage)?;
        insert_snapshot(&mut transaction, &snapshot)
            .await
            .map_err(|_| PublishError::Storage)?;
        sqlx::query(
            "UPDATE via_refresh_control
             SET current_snapshot_id = $1, publication_version = $2, lease_key = NULL, lease_token = NULL, lease_expires_at = NULL
             WHERE singleton = TRUE",
        )
        .bind(&snapshot.id)
        .bind(snapshot.publication_version as i64)
        .execute(&mut *transaction)
        .await
        .map_err(|_| PublishError::Storage)?;
        sqlx::query(
            "INSERT INTO via_refresh_cycles (refresh_key, status, snapshot_id, completed_at)
             VALUES ($1, 'published', $2, NOW())
             ON CONFLICT (refresh_key) DO UPDATE SET status = 'published', snapshot_id = EXCLUDED.snapshot_id, completed_at = EXCLUDED.completed_at",
        )
        .bind(&lease.key)
        .bind(&snapshot.id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| PublishError::Storage)?;
        sqlx::query("DELETE FROM via_live_status_observations WHERE expires_at <= NOW()")
            .execute(&mut *transaction)
            .await
            .map_err(|_| PublishError::Storage)?;
        sqlx::query("DELETE FROM via_feed_health WHERE expires_at <= NOW()")
            .execute(&mut *transaction)
            .await
            .map_err(|_| PublishError::Storage)?;
        sqlx::query(
            "DELETE FROM via_refresh_cycles WHERE completed_at < NOW() - INTERVAL '30 days'",
        )
        .execute(&mut *transaction)
        .await
        .map_err(|_| PublishError::Storage)?;
        sqlx::query(
            "DELETE FROM via_network_snapshots
             WHERE created_at < NOW() - INTERVAL '7 days'
               AND id <> $1
               AND NOT EXISTS (
                   SELECT 1 FROM via_refresh_cycles r WHERE r.snapshot_id = via_network_snapshots.id
               )",
        )
        .bind(&snapshot.id)
        .execute(&mut *transaction)
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

async fn insert_snapshot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    snapshot: &NetworkSnapshot,
) -> Result<(), sqlx::Error> {
    let feed_evidence = serde_json::to_value(&snapshot.feed_evidence)
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let selected_observation_ids = serde_json::to_value(
        snapshot
            .live_status
            .iter()
            .map(|live_status| &live_status.id)
            .collect::<Vec<_>>(),
    )
    .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    sqlx::query(
        "INSERT INTO via_network_snapshots
         (id, created_at, live_cutoff, static_network_revision, schedule_revision, freshness_policy_version, publication_version, feed_evidence, selected_observation_ids)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(&snapshot.id)
    .bind(snapshot.created_at)
    .bind(snapshot.live_cutoff)
    .bind(&snapshot.static_network_revision)
    .bind(&snapshot.schedule_revision)
    .bind(&snapshot.freshness_policy_version)
    .bind(snapshot.publication_version as i64)
    .bind(feed_evidence)
    .bind(selected_observation_ids)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn insert_live_status(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    live_status: &[NormalizedLiveStatus],
) -> Result<(), sqlx::Error> {
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
        let payload = serde_json::to_value(status)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        sqlx::query(
            "INSERT INTO via_live_status_observations
             (id, family, observed_payload, measured_at, published_at, received_at, normalized_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&status.id)
        .bind(family)
        .bind(payload)
        .bind(measured_at)
        .bind(published_at)
        .bind(status.received_at)
        .bind(status.normalized_at)
        .bind(status.normalized_at + Duration::days(7))
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn insert_feed_health(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    evidence: &[FeedEvidence],
) -> Result<(), sqlx::Error> {
    for feed in evidence {
        sqlx::query(
            "INSERT INTO via_feed_health (capability, state, reason, evaluated_at, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&feed.capability)
        .bind(freshness_state_name(feed.freshness.state))
        .bind(
            feed.failure_reason
                .map(|reason| format!("{reason:?}").to_lowercase()),
        )
        .bind(feed.freshness.evaluated_at)
        .bind(feed.freshness.evaluated_at + Duration::days(30))
        .execute(&mut **transaction)
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
        sqlx::query_scalar(
            "SELECT via_id FROM via_provider_aliases
             WHERE provider = 'tdx' AND entity_kind = $1 AND authority_namespace = $2
               AND upstream_key = $3 AND valid_from <= NOW()
               AND (valid_until IS NULL OR NOW() < valid_until)
             ORDER BY valid_from DESC LIMIT 1",
        )
        .bind(entity_kind)
        .bind(authority_namespace)
        .bind(upstream_key)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
    }
}
