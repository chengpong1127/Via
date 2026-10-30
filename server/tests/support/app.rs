use std::sync::Arc;
use via_server::domain::bikeshare::BikeAvailabilitySource;

use axum::Router;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use via_server::{
    app::build_router,
    domain::{bikeshare::BikeShareNetwork, transit::TransitNetwork},
    health::DependencyHealth,
    providers::{UnavailableBikeAvailabilitySource, tdx_bindings::TdxProviderBindings},
    routing::JourneyPlanningLimits,
    runtime::{ViaRuntime, ViaRuntimeDependencies},
    street::{StreetRouter, StreetRoutingError},
    tdx::{TdxClient, TdxCredentials},
};

fn failing_tdx_client() -> Arc<TdxClient> {
    Arc::new(
        TdxClient::new(
            TdxCredentials::from_client_credentials("fixture-client", "fixture-secret"),
            via_server::tdx::TdxSettings {
                api_base_url: "http://127.0.0.1:1/".into(),
                token_url: "http://127.0.0.1:1/".into(),
                ..via_server::config::ViaConfig::default().tdx
            },
        )
        .unwrap(),
    )
}

pub async fn build_test_router<P>(provider: Arc<P>) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    build_test_router_with_network(
        provider,
        TransitNetwork::default().with_revisions("development-static", "development-schedule"),
    )
    .await
}

pub async fn build_test_router_with_network<P>(
    provider: Arc<P>,
    transit_network: TransitNetwork,
) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    let recorder = PrometheusBuilder::new().build_recorder();
    build_test_router_with_network_and_metrics(provider, transit_network, recorder.handle()).await
}

pub async fn build_test_router_with_metrics<P>(
    provider: Arc<P>,
    metrics: PrometheusHandle,
) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    build_test_router_with_network_and_metrics(
        provider,
        TransitNetwork::default().with_revisions("development-static", "development-schedule"),
        metrics,
    )
    .await
}

pub async fn build_test_router_with_network_and_metrics<P>(
    provider: Arc<P>,
    transit_network: TransitNetwork,
    metrics: PrometheusHandle,
) -> Result<Router, StreetRoutingError>
where
    P: StreetRouter + DependencyHealth + 'static,
{
    let bike_availability_source: Arc<dyn BikeAvailabilitySource> =
        Arc::new(UnavailableBikeAvailabilitySource);
    let street: Arc<dyn StreetRouter> = provider.clone();
    let street_health: Arc<dyn DependencyHealth> = provider.clone();
    let runtime = ViaRuntime::from_dependencies(ViaRuntimeDependencies {
        environment: via_server::config::Environment::Development,
        transit_network,
        provider_bindings: TdxProviderBindings::default(),
        bike_share_network: Arc::new(
            BikeShareNetwork::new(Vec::new()).with_revision("fixture-disabled"),
        ),
        street,
        street_health,
        tdx: failing_tdx_client(),
        bike_availability: bike_availability_source,
        limits: JourneyPlanningLimits::default(),
    });
    Ok(build_router(
        runtime.routing,
        runtime.health,
        metrics,
        via_server::config::Environment::Development,
        &via_server::config::ViaConfig::default().server,
    ))
}
