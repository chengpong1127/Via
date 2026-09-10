use sea_orm::{
    ActiveValue::Set,
    ConnectionTrait, EntityTrait, TransactionTrait,
    sea_query::{Alias, Func, OnConflict, Query},
};
use sea_orm_migration::prelude::*;

use crate::entity::{
    feed_health, live_status_observation, network_snapshot, provider_alias, refresh_control,
    refresh_cycle,
};

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
            Box::new(NetworkSnapshotAuthority),
            Box::new(LiveStatusPublication),
            Box::new(RemoveRawMigrationChecksums),
            Box::new(ProductionIndexes),
        ]
    }
}

struct NetworkSnapshotAuthority;

impl MigrationName for NetworkSnapshotAuthority {
    fn name(&self) -> &str {
        "m0001_network_snapshot_authority"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for NetworkSnapshotAuthority {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .if_not_exists()
                    .table(live_status_observation::Entity)
                    .col(
                        ColumnDef::new(live_status_observation::Column::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(live_status_observation::Column::Family)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(live_status_observation::Column::ObservedPayload)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(live_status_observation::Column::MeasuredAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(live_status_observation::Column::PublishedAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(live_status_observation::Column::ReceivedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(live_status_observation::Column::NormalizedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(live_status_observation::Column::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(Expr::col(live_status_observation::Column::Family).is_in([
                        "arrival_estimate",
                        "service_condition",
                        "bike_availability",
                    ]))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .if_not_exists()
                    .table(feed_health::Entity)
                    .col(
                        ColumnDef::new(feed_health::Column::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(feed_health::Column::Capability)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(feed_health::Column::State).text().not_null())
                    .col(ColumnDef::new(feed_health::Column::Reason).text())
                    .col(
                        ColumnDef::new(feed_health::Column::EvaluatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(feed_health::Column::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .if_not_exists()
                    .table(network_snapshot::Entity)
                    .col(
                        ColumnDef::new(network_snapshot::Column::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(network_snapshot::Column::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(network_snapshot::Column::LiveCutoff)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(network_snapshot::Column::StaticNetworkRevision)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(network_snapshot::Column::ScheduleRevision)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(network_snapshot::Column::FreshnessPolicyVersion)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(network_snapshot::Column::PublicationVersion)
                            .big_integer()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(network_snapshot::Column::FeedEvidence)
                            .json_binary()
                            .not_null()
                            .default(serde_json::json!([])),
                    )
                    .check(Expr::col(network_snapshot::Column::PublicationVersion).gt(0))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .if_not_exists()
                    .table(refresh_control::Entity)
                    .col(
                        ColumnDef::new(refresh_control::Column::Singleton)
                            .boolean()
                            .not_null()
                            .default(true)
                            .primary_key(),
                    )
                    .col(ColumnDef::new(refresh_control::Column::CurrentSnapshotId).text())
                    .col(
                        ColumnDef::new(refresh_control::Column::PublicationVersion)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(refresh_control::Column::LeaseKey).text())
                    .col(ColumnDef::new(refresh_control::Column::LeaseToken).big_integer())
                    .col(
                        ColumnDef::new(refresh_control::Column::LeaseExpiresAt)
                            .timestamp_with_time_zone(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                refresh_control::Entity,
                                refresh_control::Column::CurrentSnapshotId,
                            )
                            .to(network_snapshot::Entity, network_snapshot::Column::Id),
                    )
                    .check(Expr::col(refresh_control::Column::Singleton).eq(true))
                    .to_owned(),
            )
            .await?;
        refresh_control::Entity::insert(refresh_control::ActiveModel {
            singleton: Set(true),
            current_snapshot_id: Set(None),
            publication_version: Set(0),
            lease_key: Set(None),
            lease_token: Set(None),
            lease_expires_at: Set(None),
        })
        .on_conflict(
            OnConflict::column(refresh_control::Column::Singleton)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(manager.get_connection())
        .await?;
        manager
            .create_table(
                Table::create()
                    .if_not_exists()
                    .table(refresh_cycle::Entity)
                    .col(
                        ColumnDef::new(refresh_cycle::Column::RefreshKey)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(refresh_cycle::Column::Status)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(refresh_cycle::Column::SnapshotId).text())
                    .col(
                        ColumnDef::new(refresh_cycle::Column::CompletedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(refresh_cycle::Entity, refresh_cycle::Column::SnapshotId)
                            .to(network_snapshot::Entity, network_snapshot::Column::Id),
                    )
                    .check(Expr::col(refresh_cycle::Column::Status).eq("published"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("via_live_status_expiry_idx")
                    .table(live_status_observation::Entity)
                    .col(live_status_observation::Column::ExpiresAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("via_feed_health_expiry_idx")
                    .table(feed_health::Entity)
                    .col(feed_health::Column::ExpiresAt)
                    .to_owned(),
            )
            .await
    }
}

struct LiveStatusPublication;

impl MigrationName for LiveStatusPublication {
    fn name(&self) -> &str {
        "m0002_live_status_publication"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for LiveStatusPublication {
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
        if !manager
            .has_column("via_network_snapshots", "selected_observation_ids")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(network_snapshot::Entity)
                        .add_column(
                            ColumnDef::new(network_snapshot::Column::SelectedObservationIds)
                                .json_binary()
                                .not_null()
                                .default(serde_json::json!([])),
                        )
                        .to_owned(),
                )
                .await?;
        }
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
            .await
    }
}

struct RemoveRawMigrationChecksums;

impl MigrationName for RemoveRawMigrationChecksums {
    fn name(&self) -> &str {
        "m0003_remove_raw_migration_checksums"
    }
}

struct ProductionIndexes;

impl MigrationName for ProductionIndexes {
    fn name(&self) -> &str {
        "m0004_production_indexes"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ProductionIndexes {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for index in [
            Index::create()
                .if_not_exists()
                .name("via_live_status_family_expiry_idx")
                .table(live_status_observation::Entity)
                .col(live_status_observation::Column::Family)
                .col(live_status_observation::Column::ExpiresAt)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("via_live_status_received_idx")
                .table(live_status_observation::Entity)
                .col(live_status_observation::Column::ReceivedAt)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("via_feed_health_capability_evaluated_idx")
                .table(feed_health::Entity)
                .col(feed_health::Column::Capability)
                .col(feed_health::Column::EvaluatedAt)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("via_network_snapshot_created_idx")
                .table(network_snapshot::Entity)
                .col(network_snapshot::Column::CreatedAt)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("via_network_snapshot_revisions_idx")
                .table(network_snapshot::Entity)
                .col(network_snapshot::Column::StaticNetworkRevision)
                .col(network_snapshot::Column::ScheduleRevision)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("via_refresh_cycles_completed_idx")
                .table(refresh_cycle::Entity)
                .col(refresh_cycle::Column::CompletedAt)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("via_refresh_cycles_status_idx")
                .table(refresh_cycle::Entity)
                .col(refresh_cycle::Column::Status)
                .col(refresh_cycle::Column::CompletedAt)
                .to_owned(),
        ] {
            manager.create_index(index).await?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl MigrationTrait for RemoveRawMigrationChecksums {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .if_exists()
                    .table(Alias::new("via_migration_checksums"))
                    .to_owned(),
            )
            .await
    }
}
