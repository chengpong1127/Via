use std::{env, error::Error, sync::Arc, time::Duration};

use crate::{
    data::ViaStatic,
    domain::{
        bikeshare::BikeShareNetwork, transit::TransitNetwork, transit_runtime::TransitRuntime,
    },
    health::DependencyHealth,
    observability::OperationalMetrics,
    providers::bindings::TdxProviderBindings,
    providers::{
        BusDepartureSource, DepartureDispatcher, MetroDepartureSource, TdxBikeAvailabilitySource,
        TdxServiceConditionProvider, ThsrDepartureSource, TraDepartureSource,
        TracingOperationalEventObserver, TransitDepartureSource, ValhallaConfig, ValhallaProvider,
        bikeshare::BikeAvailabilitySource,
        service_conditions::{
            CachedServiceConditionProvider, DEFAULT_SERVICE_CONDITION_CACHE_TTL,
            ServiceConditionMetrics, ServiceConditionProvider,
        },
        timing::BusTimingMetrics,
    },
    routing::{RoutingLimits, RoutingService},
    street::StreetRouter,
    tdx::{TdxClient, TdxCredentials},
};

pub struct RoutingRuntime {
    pub routing: RoutingService,
    pub transit_runtime: Arc<TransitRuntime>,
    pub service_condition_metrics: ServiceConditionMetrics,
    pub bus_timing_metrics: BusTimingMetrics,
}

pub struct RoutingRuntimeDependencies {
    pub transit_network: TransitNetwork,
    pub provider_bindings: TdxProviderBindings,
    pub bike_share_network: Arc<BikeShareNetwork>,
    pub street: Arc<dyn StreetRouter>,
    pub tdx: Option<Arc<TdxClient>>,
    pub service_conditions: Arc<dyn ServiceConditionProvider>,
    pub bike_availability: Arc<dyn BikeAvailabilitySource>,
    pub cache_ttl: Duration,
    pub limits: RoutingLimits,
}

#[derive(Clone, Debug, Default)]
pub struct ProductionRuntimeOptions {
    pub limits: RoutingLimits,
    pub valhalla_request_timeout: Option<Duration>,
}

impl RoutingRuntime {
    pub fn from_dependencies(dependencies: RoutingRuntimeDependencies) -> Self {
        let service_conditions = Arc::new(CachedServiceConditionProvider::with_ttl(
            dependencies.service_conditions,
            dependencies.cache_ttl,
        ));
        let service_condition_metrics = service_conditions.metrics();
        let bus_timing_metrics = BusTimingMetrics::default();
        let transit_runtime = Arc::new(TransitRuntime::new(Arc::new(dependencies.transit_network)));
        let provider_bindings = Arc::new(dependencies.provider_bindings);
        let bus = Arc::new(
            BusDepartureSource::new(transit_runtime.clone(), dependencies.tdx.clone())
                .with_bindings(provider_bindings.clone())
                .with_metrics(bus_timing_metrics.clone()),
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
            service_conditions,
            dependencies.bike_share_network,
            dependencies.bike_availability,
            dependencies.limits,
        );
        Self {
            routing,
            transit_runtime,
            service_condition_metrics,
            bus_timing_metrics,
        }
    }
}

pub async fn build_production_runtime(
    via_static: ViaStatic,
    options: ProductionRuntimeOptions,
) -> Result<(Arc<ValhallaProvider>, RoutingRuntime), Box<dyn Error>> {
    via_static.validate()?;
    let environment = env::var("VIA_ENV").unwrap_or_else(|_| "development".to_owned());
    let mut valhalla_config = ValhallaConfig::local();
    if let Some(timeout) = options.valhalla_request_timeout {
        valhalla_config.request_timeout = timeout;
    }
    if let Ok(base_url) = env::var("VIA_VALHALLA_URL") {
        valhalla_config.base_url = url::Url::parse(&base_url)?;
    }
    if environment == "production" && valhalla_config.base_url.scheme() != "https" {
        return Err("VIA_VALHALLA_URL must use HTTPS in production".into());
    }
    let street = Arc::new(ValhallaProvider::with_observer(
        valhalla_config,
        Arc::new(TracingOperationalEventObserver),
    )?);
    if !street.healthy().await {
        return Err("Valhalla is not healthy".into());
    }

    let tdx_base_url = env::var("VIA_TDX_BASE_URL")
        .unwrap_or_else(|_| "https://tdx.transportdata.tw/api/basic/".to_owned());
    let parsed_tdx_url = url::Url::parse(&tdx_base_url)?;
    if environment == "production" && parsed_tdx_url.scheme() != "https" {
        return Err("VIA_TDX_BASE_URL must use HTTPS in production".into());
    }
    let tdx = Arc::new(TdxClient::with_api_base_url(
        TdxCredentials::from_env()?,
        parsed_tdx_url.as_str(),
    )?);
    let bindings = via_static.provider_bindings.clone();
    let service_conditions =
        TdxServiceConditionProvider::new(tdx.clone()).with_bindings(Arc::new(bindings.clone()));
    let service_conditions = if let Ok(path) = env::var("VIA_TDX_SERVICE_CONDITION_PATH") {
        service_conditions.with_service_condition_path(path)
    } else {
        service_conditions
    };
    let cache_ttl = env::var("VIA_SERVICE_CONDITION_CACHE_TTL_SECONDS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_SERVICE_CONDITION_CACHE_TTL);
    let bike_share_network = Arc::new(via_static.bike_share);
    let bike_availability: Arc<dyn BikeAvailabilitySource> = Arc::new(
        TdxBikeAvailabilitySource::new(bike_share_network.clone(), tdx.clone()),
    );
    let runtime = RoutingRuntime::from_dependencies(RoutingRuntimeDependencies {
        transit_network: via_static.transit,
        provider_bindings: bindings,
        bike_share_network,
        street: street.clone(),
        tdx: Some(tdx),
        service_conditions: Arc::new(service_conditions),
        bike_availability,
        cache_ttl,
        limits: options.limits,
    });
    Ok((street, runtime))
}

pub fn runtime_metrics(runtime: &RoutingRuntime) -> OperationalMetrics {
    runtime.routing.operational_metrics.clone()
}
