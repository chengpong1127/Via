use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement, TransactionTrait};
use sea_orm_migration::prelude::*;
use sha2::{Digest, Sha256};

const MIGRATION_LOCK: i64 = 1_986_252_370;
const SNAPSHOT_AUTHORITY_VERSION: &str = "m0001_network_snapshot_authority";
const SNAPSHOT_AUTHORITY_SQL: &str = include_str!("../migrations/0001_snapshot_authority.sql");
const LIVE_STATUS_PUBLICATION_VERSION: &str = "m0002_live_status_publication";
const LIVE_STATUS_PUBLICATION_SQL: &str =
    include_str!("../migrations/0002_live_status_publication.sql");

pub async fn migrate(database: &DatabaseConnection) -> Result<(), DbErr> {
    let transaction = database.begin().await?;
    transaction
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [MIGRATION_LOCK.into()],
        ))
        .await?;
    Migrator::up(&transaction, None).await?;
    verify_checksum(
        &transaction,
        SNAPSHOT_AUTHORITY_VERSION,
        SNAPSHOT_AUTHORITY_SQL,
    )
    .await?;
    verify_checksum(
        &transaction,
        LIVE_STATUS_PUBLICATION_VERSION,
        LIVE_STATUS_PUBLICATION_SQL,
    )
    .await?;
    transaction.commit().await
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(NetworkSnapshotAuthority),
            Box::new(LiveStatusPublication),
        ]
    }
}

struct NetworkSnapshotAuthority;

impl MigrationName for NetworkSnapshotAuthority {
    fn name(&self) -> &str {
        SNAPSHOT_AUTHORITY_VERSION
    }
}

#[async_trait::async_trait]
impl MigrationTrait for NetworkSnapshotAuthority {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        apply_sql_migration(manager, SNAPSHOT_AUTHORITY_VERSION, SNAPSHOT_AUTHORITY_SQL).await
    }
}

struct LiveStatusPublication;

impl MigrationName for LiveStatusPublication {
    fn name(&self) -> &str {
        LIVE_STATUS_PUBLICATION_VERSION
    }
}

#[async_trait::async_trait]
impl MigrationTrait for LiveStatusPublication {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        apply_sql_migration(
            manager,
            LIVE_STATUS_PUBLICATION_VERSION,
            LIVE_STATUS_PUBLICATION_SQL,
        )
        .await
    }
}

async fn apply_sql_migration(
    manager: &SchemaManager<'_>,
    version: &str,
    sql: &str,
) -> Result<(), DbErr> {
    let connection = manager.get_connection();
    connection.execute_unprepared(sql).await?;
    connection
        .execute_unprepared(
            "CREATE TABLE IF NOT EXISTS via_migration_checksums (
                version TEXT PRIMARY KEY,
                checksum TEXT NOT NULL
            )",
        )
        .await?;
    connection
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO via_migration_checksums (version, checksum) VALUES ($1, $2)",
            [version.into(), checksum(sql).into()],
        ))
        .await?;
    Ok(())
}

async fn verify_checksum(
    connection: &impl ConnectionTrait,
    version: &str,
    sql: &str,
) -> Result<(), DbErr> {
    let row = connection
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT checksum FROM via_migration_checksums WHERE version = $1",
            [version.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom(format!("missing checksum for migration {version}")))?;
    let stored: String = row.try_get("", "checksum")?;
    if stored != checksum(sql) {
        return Err(DbErr::Custom(format!(
            "checksum mismatch for migration {version}"
        )));
    }
    Ok(())
}

fn checksum(sql: &str) -> String {
    format!("{:x}", Sha256::digest(sql.as_bytes()))
}
