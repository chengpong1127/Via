use time::{Duration, OffsetDateTime};
use via_server::providers::postgres::{PostgresIdentityResolver, ProviderAliasRecord};
use via_server::providers::tdx::IdentityResolver;

#[tokio::test]
#[ignore = "requires VIA_TEST_DATABASE_URL pointing to disposable PostgreSQL"]
async fn postgres_persists_bidirectional_provider_aliases() {
    let database_url = std::env::var("VIA_TEST_DATABASE_URL").unwrap();
    let resolver = PostgresIdentityResolver::connect(&database_url, 2)
        .await
        .unwrap();
    let valid_from = OffsetDateTime::now_utc() - Duration::seconds(1);
    let suffix = valid_from.unix_timestamp_nanos().to_string();
    let upstream_key = format!("provider-service-{suffix}");
    let via_id = format!("via-service-{suffix}");
    resolver
        .upsert_provider_aliases(&[ProviderAliasRecord {
            provider: "tdx".into(),
            entity_kind: "transit_service".into(),
            authority_namespace: "taipei".into(),
            upstream_key: upstream_key.clone(),
            via_id: via_id.clone(),
            valid_from,
            valid_until: None,
        }])
        .await
        .unwrap();

    assert_eq!(
        resolver
            .resolve("transit_service", "taipei", &upstream_key)
            .await,
        Some(via_id.clone())
    );
    assert_eq!(
        resolver
            .upstream_key("transit_service", "taipei", &via_id)
            .await,
        Some(upstream_key)
    );
}
