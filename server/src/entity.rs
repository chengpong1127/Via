pub mod live_status_observation {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "via_live_status_observations")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub family: String,
        pub observed_payload: Json,
        pub measured_at: Option<OffsetDateTime>,
        pub published_at: Option<OffsetDateTime>,
        pub received_at: OffsetDateTime,
        pub normalized_at: OffsetDateTime,
        pub expires_at: OffsetDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod feed_health {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "via_feed_health")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub capability: String,
        pub state: String,
        pub reason: Option<String>,
        pub evaluated_at: OffsetDateTime,
        pub expires_at: OffsetDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod network_snapshot {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "via_network_snapshots")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub created_at: OffsetDateTime,
        pub live_cutoff: OffsetDateTime,
        pub static_network_revision: String,
        pub schedule_revision: String,
        pub freshness_policy_version: String,
        pub publication_version: i64,
        pub feed_evidence: Json,
        pub selected_observation_ids: Json,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod refresh_control {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "via_refresh_control")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub singleton: bool,
        pub current_snapshot_id: Option<String>,
        pub publication_version: i64,
        pub lease_key: Option<String>,
        pub lease_token: Option<i64>,
        pub lease_expires_at: Option<OffsetDateTime>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod refresh_cycle {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "via_refresh_cycles")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub refresh_key: String,
        pub status: String,
        pub snapshot_id: Option<String>,
        pub completed_at: OffsetDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod provider_alias {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "via_provider_aliases")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub provider: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub entity_kind: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub authority_namespace: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub upstream_key: String,
        pub via_id: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub valid_from: OffsetDateTime,
        pub valid_until: Option<OffsetDateTime>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
