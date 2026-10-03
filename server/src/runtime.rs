use std::{error::Error, sync::Arc, time::Duration};

use crate::{
    config::ProductionRuntimeConfig,
    data::ViaStatic,
    domain::{
        bikeshare::BikeShareNetwork, transit::TransitNetwork, transit_runtime::TransitRuntime,
    },
    health::{DependencyHealth, Health},
    providers::tdx_bindings::TdxProviderBindings,
    providers::{
        BusDepartureSource, DepartureDispatcher, MetroDepartureSource, TdxBikeAvailabilitySource,
        ThsrDepartureSource, TraDepartureSource, TracingOperationalEventObserver,
        TransitDepartureSource, ValhallaConfig, ValhallaProvider,
        bikeshare::BikeAvailabilitySource,
    },
    routing::{RoutingLimits, RoutingService},
    street::StreetRouter,
    tdx::TdxClient,
};

pub struct RoutingRuntime {
    pub routing: RoutingService,
    pub health: Health,
}

pub struct RoutingRuntimeDependencies {
    pub transit_network: TransitNetwork,
    pub provider_bindings: TdxProviderBindings,
    pub bike_share_network: Arc<BikeShareNetwork>,
    pub street: Arc<dyn StreetRouter>,
    pub street_health: Arc<dyn DependencyHealth>,
    pub tdx: Option<Arc<TdxClient>>,
    pub bike_availability: Arc<dyn BikeAvailabilitySource>,
    pub limits: RoutingLimits,
}

#[derive(Clone, Debug, Default)]
pub struct ProductionRuntimeOptions {
    pub limits: RoutingLimits,
    pub valhalla_request_timeout: Option<Duration>,
}

impl RoutingRuntime {
    pub fn from_dependencies(dependencies: RoutingRuntimeDependencies) -> Self {
        let transit_runtime = Arc::new(TransitRuntime::new(Arc::new(dependencies.transit_network)));
        let provider_bindings = Arc::new(dependencies.provider_bindings);
        let bus = Arc::new(
            BusDepartureSource::new(transit_runtime.clone(), dependencies.tdx.clone())
                .with_bindings(provider_bindings.clone()),
        );
        let metro = Arc::new(
            MetroDepartureSource::new(transit_runtime.clone())
                .with_bindings(provider_bindings.clone())
                .with_tdx(dependencies.tdx.clone()),
        );
        let tra = Arc::new(
            TraDepartureSource::new(transit_runtime.clone())
                .with_bindings(provider_bindings)
                .with_tdx(dependencies.tdx),
        );
        let thsr = Arc::new(ThsrDepartureSource::new(transit_runtime.clone()));
        let departure_resolver = Arc::new(DepartureDispatcher::new(
            transit_runtime.clone(),
            vec![
                bus as Arc<dyn TransitDepartureSource>,
                metro as Arc<dyn TransitDepartureSource>,
                tra as Arc<dyn TransitDepartureSource>,
                thsr as Arc<dyn TransitDepartureSource>,
            ],
        ));
        let routing = RoutingService::new(
            dependencies.street,
            transit_runtime.clone(),
            departure_resolver,
            dependencies.bike_share_network,
            dependencies.bike_availability,
            dependencies.limits,
        );
        let health = Health::new(dependencies.street_health, transit_runtime);
        Self { routing, health }
    }
}

pub async fn build_production_runtime(
    via_static: ViaStatic,
    options: ProductionRuntimeOptions,
    config: ProductionRuntimeConfig,
) -> Result<RoutingRuntime, Box<dyn Error>> {
    via_static.validate()?;
    let mut valhalla_config = ValhallaConfig::local();
    if let Some(timeout) = options.valhalla_request_timeout {
        valhalla_config.request_timeout = timeout;
    }
    valhalla_config.base_url = config.valhalla.base_url;
    let street = Arc::new(ValhallaProvider::with_observer(
        valhalla_config,
        Arc::new(TracingOperationalEventObserver),
    )?);
    if !street.healthy().await {
        return Err("Valhalla is not healthy".into());
    }

    let tdx = Arc::new(TdxClient::with_api_base_url(
        config.tdx.credentials,
        config.tdx.api_base_url.as_str(),
    )?);
    let bindings = via_static.provider_bindings.clone();
    let bike_share_network = Arc::new(via_static.bike_share);
    let bike_availability: Arc<dyn BikeAvailabilitySource> = Arc::new(
        TdxBikeAvailabilitySource::new(bike_share_network.clone(), tdx.clone()),
    );
    let runtime = RoutingRuntime::from_dependencies(RoutingRuntimeDependencies {
        transit_network: via_static.transit,
        provider_bindings: bindings,
        bike_share_network,
        street: street.clone(),
        street_health: street.clone(),
        tdx: Some(tdx),
        bike_availability,
        limits: options.limits,
    });
    Ok(runtime)
}
