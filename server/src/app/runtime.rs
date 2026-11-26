use std::{error::Error, sync::Arc};

use super::{health::Health, nearby::NearbyService};
use crate::{
    config::RuntimeConfig,
    data::ViaStatic,
    domain::{
        bikeshare::{BikeAvailabilitySource, BikeShareNetwork},
        transit::{network::TransitNetwork, runtime::TransitRuntime},
    },
    live::{CachedLiveData, DirectLiveData, GatewayLiveData, LiveDataSource},
    providers::{
        BusDepartureSource, DepartureResolver, MetroDepartureSource, TdxBikeAvailabilitySource,
        TdxTimingContext, ThsrDepartureSource, TraDepartureSource, TracingOperationalEventObserver,
        TransitDepartureSource, ValhallaProvider, tdx_bindings::TdxProviderBindings,
    },
    routing::{JourneyPlanningLimits, RoutingService},
    street::{DependencyHealth, MonitoredStreetRouter},
    tdx::{RealtimeSettings, TdxClient, TdxCredentials, TdxSettings},
};

pub struct ViaRuntime {
    pub routing: RoutingService,
    pub health: Health,
    pub nearby: NearbyService,
}

pub struct ViaRuntimeDependencies {
    pub transit_network: TransitNetwork,
    pub provider_bindings: TdxProviderBindings,
    pub bike_share_network: Arc<BikeShareNetwork>,
    pub street: Arc<dyn MonitoredStreetRouter>,
    pub live: Arc<dyn LiveDataSource>,
    pub realtime: RealtimeSettings,
    pub bike_availability: Arc<dyn BikeAvailabilitySource>,
    pub limits: JourneyPlanningLimits,
}

impl ViaRuntime {
    pub fn from_dependencies(dependencies: ViaRuntimeDependencies) -> Self {
        let transit = Arc::new(TransitRuntime::new(Arc::new(dependencies.transit_network)));
        let context = TdxTimingContext::new(
            transit.clone(),
            dependencies.live,
            &dependencies.realtime,
            Arc::new(dependencies.provider_bindings),
        );
        let sources: Vec<Arc<dyn TransitDepartureSource>> = vec![
            Arc::new(BusDepartureSource::new(context.clone())),
            Arc::new(MetroDepartureSource::new(context.clone())),
            Arc::new(TraDepartureSource::new(context)),
            Arc::new(ThsrDepartureSource::new(transit.clone())),
        ];
        let departure_resolver = Arc::new(DepartureResolver::new(transit.clone(), sources));
        let nearby = NearbyService::new(
            transit.clone(),
            dependencies.bike_share_network.clone(),
            dependencies.bike_availability.clone(),
        );
        let health = Health::new(dependencies.street.clone(), &transit);
        let routing = RoutingService::new(
            dependencies.street,
            transit,
            departure_resolver,
            dependencies.bike_share_network,
            dependencies.bike_availability,
            dependencies.limits,
        );
        Self {
            routing,
            health,
            nearby,
        }
    }
}

pub async fn build_runtime(
    via_static: ViaStatic,
    config: RuntimeConfig,
) -> Result<ViaRuntime, Box<dyn Error>> {
    via_static.validate()?;
    let street = Arc::new(ValhallaProvider::with_observer(
        config.settings.valhalla.provider_config(false)?,
        Arc::new(TracingOperationalEventObserver),
    )?);
    if !street.healthy().await {
        return Err("Valhalla is not healthy".into());
    }

    let live = live_data_source(config.tdx, &config.settings.tdx)?;
    let bike_availability: Arc<dyn BikeAvailabilitySource> = Arc::new(
        TdxBikeAvailabilitySource::new(live.clone(), config.settings.tdx.bikeshare.clone()),
    );
    Ok(ViaRuntime::from_dependencies(ViaRuntimeDependencies {
        transit_network: via_static.transit,
        provider_bindings: via_static.provider_bindings,
        bike_share_network: Arc::new(via_static.bike_share),
        street,
        live,
        realtime: config.settings.tdx.realtime.clone(),
        bike_availability,
        limits: config.settings.journey_limits(config.environment),
    }))
}

pub fn live_data_source(
    credentials: Option<TdxCredentials>,
    settings: &TdxSettings,
) -> Result<Arc<dyn LiveDataSource>, Box<dyn Error>> {
    if let Some(url) = &settings.gateway.url {
        return Ok(Arc::new(GatewayLiveData::new(url, settings)?));
    }
    let credentials =
        credentials.ok_or("TDX credentials are required when tdx.gateway.url is not set")?;
    Ok(direct_live_data(credentials, settings)?)
}

pub fn direct_live_data(
    credentials: TdxCredentials,
    settings: &TdxSettings,
) -> Result<Arc<CachedLiveData>, Box<dyn Error>> {
    let client = Arc::new(TdxClient::new(credentials, settings.clone())?);
    Ok(Arc::new(CachedLiveData::new(
        Arc::new(DirectLiveData::new(client)),
        settings,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::PlanningRequest,
        app::build_router,
        config::Environment,
        domain::journey::{PlanningError, PlanningOutcome},
        providers::UnavailableBikeAvailabilitySource,
        street::{
            MatrixQuery, MatrixResult, RouteQuery, StraightLineStreetRouter, StreetPath,
            StreetRouter, StreetRoutingError,
        },
        tdx::TdxCredentials,
    };
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use std::time::Duration;
    use tower::ServiceExt;

    struct SlowStreetRouter;

    #[async_trait]
    impl StreetRouter for SlowStreetRouter {
        async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
            StraightLineStreetRouter::default().matrix(query).await
        }

        async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
            tokio::time::sleep(Duration::from_secs(16)).await;
            StraightLineStreetRouter::default().route(query).await
        }
    }

    #[async_trait]
    impl DependencyHealth for SlowStreetRouter {
        async fn healthy(&self) -> bool {
            tokio::time::sleep(Duration::from_secs(16)).await;
            true
        }

        fn name(&self) -> &'static str {
            "slow-test-router"
        }
    }

    fn runtime(environment: Environment) -> ViaRuntime {
        let street = Arc::new(SlowStreetRouter);
        ViaRuntime::from_dependencies(ViaRuntimeDependencies {
            transit_network: TransitNetwork::default().with_revisions("static", "schedule"),
            provider_bindings: TdxProviderBindings::default(),
            bike_share_network: Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("test")),
            street,
            live: Arc::new(DirectLiveData::new(Arc::new(
                TdxClient::new(
                    TdxCredentials::from_client_credentials("test", "test"),
                    crate::tdx::TdxSettings {
                        api_base_url: "http://127.0.0.1:1/".into(),
                        token_url: "http://127.0.0.1:1/".into(),
                        ..crate::config::ViaConfig::default().tdx
                    },
                )
                .unwrap(),
            ))),
            realtime: crate::config::ViaConfig::default().tdx.realtime,
            bike_availability: Arc::new(UnavailableBikeAvailabilitySource),
            limits: crate::config::ViaConfig::default().journey_limits(environment),
        })
    }

    fn request() -> PlanningRequest {
        serde_json::from_value(serde_json::json!({
            "origin": {"coordinate": {"latitude": 25.0, "longitude": 121.5}, "display_text": "Origin"},
            "destination": {"coordinate": {"latitude": 25.001, "longitude": 121.501}, "display_text": "Destination"},
            "selected_modes": [],
            "departure_setting": {"type": "depart_now"}
        })).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn development_planning_waits_past_request_and_phase2_timeouts() {
        let runtime = runtime(Environment::Development);
        let started = tokio::time::Instant::now();
        let result = runtime.routing.plan(request().into()).await.unwrap();
        assert!(matches!(result, PlanningOutcome::Options(_)));
        assert!(started.elapsed() >= Duration::from_secs(16));
    }

    #[tokio::test(start_paused = true)]
    async fn production_planning_keeps_the_ten_second_timeout() {
        let runtime = runtime(Environment::Production);
        let started = tokio::time::Instant::now();
        assert!(matches!(
            runtime.routing.plan(request().into()).await,
            Err(PlanningError::TemporarilyUnavailable)
        ));
        assert_eq!(started.elapsed(), Duration::from_secs(10));
    }

    async fn slow_http_response(environment: Environment) -> StatusCode {
        let runtime = runtime(environment);
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let settings = crate::config::ViaConfig::default().server;
        let router = build_router(
            runtime,
            recorder.handle(),
            settings.http_timeout(environment),
            settings.max_body_bytes,
        );
        router
            .oneshot(
                Request::builder()
                    .uri("/health/street-routing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test(start_paused = true)]
    async fn development_router_waits_past_the_http_timeout() {
        let started = tokio::time::Instant::now();
        assert_eq!(
            slow_http_response(Environment::Development).await,
            StatusCode::OK
        );
        assert_eq!(started.elapsed(), Duration::from_secs(16));
    }

    #[tokio::test(start_paused = true)]
    async fn production_router_keeps_the_fifteen_second_http_timeout() {
        let started = tokio::time::Instant::now();
        assert_eq!(
            slow_http_response(Environment::Production).await,
            StatusCode::REQUEST_TIMEOUT
        );
        assert_eq!(started.elapsed(), Duration::from_secs(15));
    }
}
