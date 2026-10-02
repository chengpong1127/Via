use std::sync::Arc;

use axum::Router;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use via_server::{
    app::build_router,
    domain::{bikeshare::BikeShareNetwork, transit::TransitNetwork},
    health::DependencyHealth,
    providers::{
        BikeAvailabilitySource, UnavailableBikeAvailabilitySource,
        tdx_bindings::TdxProviderBindings,
    },
    routing::RoutingLimits,
    runtime::{RoutingRuntime, RoutingRuntimeDependencies},
    street::{StreetRouter, StreetRoutingError},
};

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
    let runtime = RoutingRuntime::from_dependencies(RoutingRuntimeDependencies {
        transit_network,
        provider_bindings: TdxProviderBindings::default(),
        bike_share_network: Arc::new(
            BikeShareNetwork::new(Vec::new()).with_revision("fixture-disabled"),
        ),
        street,
        tdx: None,
        bike_availability: bike_availability_source,
        limits: RoutingLimits::default(),
    });
    Ok(build_router(provider, runtime, metrics))
}
