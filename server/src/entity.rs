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
