use sea_orm::{
    ConnectionTrait, TransactionTrait,
    sea_query::{Alias, Func, Query},
};
use sea_orm_migration::prelude::*;

use crate::entity::provider_alias;

pub async fn migrate(database: &sea_orm::DatabaseConnection) -> Result<(), DbErr> {
    let transaction = database.begin().await?;
    let lock = Query::select()
        .expr(Func::cust(Alias::new("pg_advisory_xact_lock")).arg(1_986_252_370_i64))
        .to_owned();
    transaction
        .query_one(transaction.get_database_backend().build(&lock))
        .await?;
    Migrator::up(&transaction, None).await?;
    transaction.commit().await
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(ProviderAliasRegistry),
            Box::new(RemoveLegacyRealtimeTables),
        ]
    }
}

struct ProviderAliasRegistry;

impl MigrationName for ProviderAliasRegistry {
    fn name(&self) -> &str {
        "m0001_provider_alias_registry"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ProviderAliasRegistry {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .if_not_exists()
                    .table(provider_alias::Entity)
                    .col(
                        ColumnDef::new(provider_alias::Column::Provider)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(provider_alias::Column::EntityKind)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(provider_alias::Column::AuthorityNamespace)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(provider_alias::Column::UpstreamKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(provider_alias::Column::ViaId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(provider_alias::Column::ValidFrom)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(provider_alias::Column::ValidUntil)
                            .timestamp_with_time_zone(),
                    )
                    .primary_key(
                        Index::create()
                            .col(provider_alias::Column::Provider)
                            .col(provider_alias::Column::EntityKind)
                            .col(provider_alias::Column::AuthorityNamespace)
                            .col(provider_alias::Column::UpstreamKey)
                            .col(provider_alias::Column::ValidFrom),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("via_provider_alias_lookup_idx")
                    .table(provider_alias::Entity)
                    .col(provider_alias::Column::Provider)
                    .col(provider_alias::Column::EntityKind)
                    .col(provider_alias::Column::AuthorityNamespace)
                    .col(provider_alias::Column::UpstreamKey)
                    .col(provider_alias::Column::ValidFrom)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("via_provider_alias_reverse_lookup_idx")
                    .table(provider_alias::Entity)
                    .col(provider_alias::Column::Provider)
                    .col(provider_alias::Column::EntityKind)
                    .col(provider_alias::Column::AuthorityNamespace)
                    .col(provider_alias::Column::ViaId)
                    .col(provider_alias::Column::ValidFrom)
                    .to_owned(),
            )
            .await
    }
}

struct RemoveLegacyRealtimeTables;

impl MigrationName for RemoveLegacyRealtimeTables {
    fn name(&self) -> &str {
        "m0002_remove_legacy_realtime_tables"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for RemoveLegacyRealtimeTables {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            "via_refresh_cycles",
            "via_refresh_control",
            "via_network_snapshots",
            "via_feed_health",
            "via_live_status_observations",
        ] {
            manager
                .drop_table(
                    Table::drop()
                        .if_exists()
                        .table(Alias::new(table))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
