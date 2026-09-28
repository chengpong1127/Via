use std::sync::Arc;

use axum::Router;

use via_server::{
    app::build_router,
    domain::{bikeshare::BikeShareNetwork, transit::TransitNetwork},
    health::DependencyHealth,
    providers::{BikeAvailabilitySource, UnavailableBikeAvailabilitySource},
    providers::{
        bindings::TdxProviderBindings,
        service_conditions::{DEFAULT_SERVICE_CONDITION_CACHE_TTL, NullServiceConditionProvider},
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
    let bike_availability_source: Arc<dyn BikeAvailabilitySource> =
        Arc::new(UnavailableBikeAvailabilitySource);
    let street: Arc<dyn StreetRouter> = provider.clone();
    let runtime = RoutingRuntime::from_dependencies(RoutingRuntimeDependencies {
        transit_network,
        provider_bindings: TdxProviderBindings::default(),
        bike_share_network: BikeShareNetwork::new(Vec::new()).with_revision("fixture-disabled"),
        street,
        tdx: None,
        service_conditions: Arc::new(NullServiceConditionProvider),
        bike_availability: bike_availability_source,
        cache_ttl: DEFAULT_SERVICE_CONDITION_CACHE_TTL,
        limits: RoutingLimits::default(),
    });
    Ok(build_router(provider, runtime))
}
