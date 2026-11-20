use std::sync::Arc;
use via_server::domain::bikeshare::BikeAvailabilitySource;

use axum::Router;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use via_server::{
    app::{
        build_router,
        runtime::{ViaRuntime, ViaRuntimeDependencies},
    },
    domain::{bikeshare::BikeShareNetwork, transit::network::TransitNetwork},
    providers::{UnavailableBikeAvailabilitySource, tdx_bindings::TdxProviderBindings},
    routing::JourneyPlanningLimits,
    street::{DependencyHealth, StreetRouter, StreetRoutingError},
};

#[path = "street.rs"]
pub mod street;
#[allow(dead_code)]
#[path = "tdx.rs"]
pub mod tdx;

use tdx::failing_tdx_client;

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
    let runtime = ViaRuntime::from_dependencies(ViaRuntimeDependencies {
        transit_network,
        provider_bindings: TdxProviderBindings::default(),
        bike_share_network: Arc::new(
            BikeShareNetwork::new(Vec::new()).with_revision("fixture-disabled"),
        ),
        street: provider,
        live: Arc::new(via_server::live::DirectLiveData::new(failing_tdx_client())),
        realtime: via_server::config::ViaConfig::default().tdx.realtime,
        bike_availability: bike_availability_source,
        limits: JourneyPlanningLimits::default(),
    });
    Ok(build_router(
        runtime,
        metrics,
        None,
        via_server::config::ViaConfig::default()
            .server
            .max_body_bytes,
    ))
}
