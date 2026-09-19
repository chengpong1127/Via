use std::sync::Arc;

use axum::Router;

use via_server::{
    app::{RouterDependencies, build_router},
    domain::{bikeshare::BikeShareNetwork, transit::TransitNetwork},
    health::DependencyHealth,
    providers::service_conditions::NullServiceConditionProvider,
    providers::{BikeAvailabilitySource, UnavailableBikeAvailabilitySource},
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
    build_router(
        provider,
        RouterDependencies::new(
            Arc::new(NullServiceConditionProvider),
            Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("fixture-disabled")),
            bike_availability_source,
        ),
        transit_network,
    )
    .await
}
