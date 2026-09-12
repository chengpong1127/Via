use std::time::Duration as StdDuration;

use async_trait::async_trait;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectOptions, Database, DatabaseConnection, DbErr,
    EntityTrait, QueryFilter, QueryOrder, TransactionTrait, sea_query::OnConflict,
};
use time::OffsetDateTime;

use crate::{entity::provider_alias, migration, tdx::IdentityResolver};

#[derive(Clone)]
pub struct PostgresIdentityResolver {
    database: DatabaseConnection,
}

impl PostgresIdentityResolver {
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, DbErr> {
        let mut options = ConnectOptions::new(database_url);
        options
            .max_connections(max_connections.max(1))
            .acquire_timeout(StdDuration::from_secs(5));
        let database = Database::connect(options).await?;
        migration::migrate(&database).await?;
        Ok(Self { database })
    }

    /// Install reviewed provider aliases for the immutable static-network artifact.
    /// Alias rows are versioned independently from realtime values.
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
impl IdentityResolver for PostgresIdentityResolver {
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

    async fn upstream_key(
        &self,
        entity_kind: &str,
        authority_namespace: &str,
        via_id: &str,
    ) -> Option<String> {
        let now = OffsetDateTime::now_utc();
        provider_alias::Entity::find()
            .filter(provider_alias::Column::Provider.eq("tdx"))
            .filter(provider_alias::Column::EntityKind.eq(entity_kind))
            .filter(provider_alias::Column::AuthorityNamespace.eq(authority_namespace))
            .filter(provider_alias::Column::ViaId.eq(via_id))
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
            .map(|alias| alias.upstream_key)
    }
}
