use std::{error::Error, sync::Arc, time::Duration};

use crate::{
    config::{Environment, RuntimeConfig},
    data::ViaStatic,
    domain::{
        bikeshare::BikeShareNetwork, transit::TransitNetwork, transit_runtime::TransitRuntime,
    },
    health::{DependencyHealth, Health},
    providers::tdx_bindings::TdxProviderBindings,
    providers::{
        BusDepartureSource, DepartureResolver, MetroDepartureSource, TdxBikeAvailabilitySource,
        ThsrDepartureSource, TraDepartureSource, TracingOperationalEventObserver,
        TransitDepartureSource, ValhallaConfig, ValhallaProvider,
        bikeshare::BikeAvailabilitySource,
    },
    routing::{JourneyPlanningLimits, RoutingService},
    street::StreetRouter,
    tdx::TdxClient,
};

pub struct ViaRuntime {
    pub routing: RoutingService,
    pub health: Health,
}

pub struct ViaRuntimeDependencies {
    pub environment: Environment,
    pub transit_network: TransitNetwork,
    pub provider_bindings: TdxProviderBindings,
    pub bike_share_network: Arc<BikeShareNetwork>,
    pub street: Arc<dyn StreetRouter>,
    pub street_health: Arc<dyn DependencyHealth>,
    pub tdx: Arc<TdxClient>,
    pub bike_availability: Arc<dyn BikeAvailabilitySource>,
    pub limits: JourneyPlanningLimits,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeOptions {
    pub limits: JourneyPlanningLimits,
    pub valhalla_request_timeout: Option<Duration>,
}

impl ViaRuntime {
    pub fn from_dependencies(dependencies: ViaRuntimeDependencies) -> Self {
        let transit_runtime = Arc::new(TransitRuntime::new(Arc::new(dependencies.transit_network)));
        let provider_bindings = Arc::new(dependencies.provider_bindings);
        let bus = Arc::new(
            BusDepartureSource::new(transit_runtime.clone(), dependencies.tdx.clone())
                .with_bindings(provider_bindings.clone()),
        );
        let metro = Arc::new(
            MetroDepartureSource::new(transit_runtime.clone(), dependencies.tdx.clone())
                .with_bindings(provider_bindings.clone()),
        );
        let tra = Arc::new(
            TraDepartureSource::new(transit_runtime.clone(), dependencies.tdx.clone())
                .with_bindings(provider_bindings),
        );
        let thsr = Arc::new(ThsrDepartureSource::new(transit_runtime.clone()));
        let departure_resolver = Arc::new(DepartureResolver::new(
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
            dependencies.environment.is_production(),
        );
        let health = Health::new(dependencies.street_health, transit_runtime);
        Self { routing, health }
    }
}

pub async fn build_runtime(
    via_static: ViaStatic,
    options: RuntimeOptions,
    config: RuntimeConfig,
) -> Result<ViaRuntime, Box<dyn Error>> {
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
    let bike_availability: Arc<dyn BikeAvailabilitySource> =
        Arc::new(TdxBikeAvailabilitySource::new(tdx.clone()));
    let runtime = ViaRuntime::from_dependencies(ViaRuntimeDependencies {
        environment: config.environment,
        transit_network: via_static.transit,
        provider_bindings: bindings,
        bike_share_network,
        street: street.clone(),
        street_health: street.clone(),
        tdx,
        bike_availability,
        limits: options.limits,
    });
    Ok(runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::PlanningRequest,
        app::build_router,
        domain::journey::{PlanningError, PlanningOutcome},
        providers::UnavailableBikeAvailabilitySource,
        street::{
            MatrixQuery, MatrixResult, RouteQuery, StraightLineStreetRouter, StreetPath,
            StreetRoutingError,
        },
        tdx::TdxCredentials,
    };
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    struct SlowStreetRouter;

    #[async_trait]
    impl StreetRouter for SlowStreetRouter {
        async fn matrix(&self, query: MatrixQuery) -> Result<MatrixResult, StreetRoutingError> {
            StraightLineStreetRouter::default().matrix(query).await
        }

        async fn route(&self, query: RouteQuery) -> Result<StreetPath, StreetRoutingError> {
            tokio::time::sleep(Duration::from_secs(11)).await;
            StraightLineStreetRouter::default().route(query).await
        }
    }

    #[async_trait]
    impl DependencyHealth for SlowStreetRouter {
        async fn healthy(&self) -> bool {
            tokio::time::sleep(Duration::from_secs(11)).await;
            true
        }

        fn name(&self) -> &'static str {
            "slow-test-router"
        }
    }

    fn runtime(environment: Environment) -> ViaRuntime {
        let street = Arc::new(SlowStreetRouter);
        ViaRuntime::from_dependencies(ViaRuntimeDependencies {
            environment,
            transit_network: TransitNetwork::default().with_revisions("static", "schedule"),
            provider_bindings: TdxProviderBindings::default(),
            bike_share_network: Arc::new(BikeShareNetwork::new(Vec::new()).with_revision("test")),
            street: street.clone(),
            street_health: street,
            tdx: Arc::new(
                TdxClient::with_urls(
                    TdxCredentials::from_client_credentials("test", "test"),
                    "http://127.0.0.1:1/",
                    "http://127.0.0.1:1/",
                )
                .unwrap(),
            ),
            bike_availability: Arc::new(UnavailableBikeAvailabilitySource),
            limits: JourneyPlanningLimits::default(),
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
        let result = runtime.routing.plan(request()).await.unwrap();
        assert!(matches!(result, PlanningOutcome::Options(_)));
        assert!(started.elapsed() >= Duration::from_secs(11));
    }

    #[tokio::test(start_paused = true)]
    async fn production_planning_keeps_the_five_second_timeout() {
        let runtime = runtime(Environment::Production);
        let started = tokio::time::Instant::now();
        assert!(matches!(
            runtime.routing.plan(request()).await,
            Err(PlanningError::TemporarilyUnavailable)
        ));
        assert_eq!(started.elapsed(), Duration::from_secs(5));
    }

    async fn slow_http_response(environment: Environment) -> StatusCode {
        let runtime = runtime(environment);
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let router = build_router(
            runtime.routing,
            runtime.health,
            recorder.handle(),
            environment,
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
        assert_eq!(started.elapsed(), Duration::from_secs(11));
    }

    #[tokio::test(start_paused = true)]
    async fn production_router_keeps_the_ten_second_http_timeout() {
        let started = tokio::time::Instant::now();
        assert_eq!(
            slow_http_response(Environment::Production).await,
            StatusCode::REQUEST_TIMEOUT
        );
        assert_eq!(started.elapsed(), Duration::from_secs(10));
    }
}
