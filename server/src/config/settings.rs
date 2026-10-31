use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use super::{ConfigError, Environment};
use crate::{
    data::bike_access::BikeAccessPolicy,
    domain::ranking::RankingPolicy,
    providers::valhalla::ValhallaConfig,
    routing::{JourneyPlanningLimits, PlannerLimits},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ViaConfig {
    pub server: ServerSettings,
    pub routing: RoutingSettings,
    pub bikeshare: BikeShareSettings,
    pub preprocessing: PreprocessingSettings,
    pub tdx: TdxSettings,
    pub valhalla: ValhallaSettings,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSettings {
    pub port: u16,
    pub static_path: Option<PathBuf>,
    pub request_timeout_ms: u64,
    pub http_timeout_ms: u64,
    pub max_concurrent_requests: usize,
    pub max_body_bytes: usize,
}

fn default_phase2_corridors() -> usize {
    8
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingSettings {
    pub max_transfers: usize,
    pub max_static_corridors: usize,
    pub max_concurrent_phase2_states: usize,
    #[serde(default = "default_phase2_corridors")]
    pub max_concurrent_phase2_corridors: usize,
    pub max_concurrent_reconstruction_candidates: usize,
    pub max_candidates: usize,
    pub max_departures_per_query: usize,
    pub max_journey_duration_seconds: u32,
    pub access: AccessSettings,
    pub ranking: RankingPolicy,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessSettings {
    pub max_transit_access_stops: usize,
    pub max_transit_egress_stops: usize,
    pub max_walking_radius_metres: u32,
    pub max_walking_seconds: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BikeShareSettings {
    pub max_pickup_stations_per_anchor: usize,
    pub max_dropoff_stations_per_anchor: usize,
    pub max_stations_per_endpoint: usize,
    pub endpoint_radius_metres: u32,
    pub max_endpoint_walk_seconds: u32,
    pub max_bike_duration_seconds: u32,
    pub max_bike_distance_metres: u32,
    pub optimistic_walk_speed_metres_per_second: f64,
    pub preprocessing: BikeAccessPolicy,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreprocessingSettings {
    pub same_stop_transfer_seconds: u32,
    pub transfers: TransferSettings,
}

pub use crate::data::static_transfers::TransferSettings;

pub use crate::tdx::{BikeAvailabilitySettings, RealtimeSettings, TdxBatchSettings, TdxSettings};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValhallaSettings {
    pub base_url: String,
    pub request_timeout_ms: u64,
    pub build_request_timeout_ms: u64,
    pub max_concurrency: usize,
    pub max_sources: usize,
    pub max_targets: usize,
    pub max_response_bytes: usize,
}

impl Default for ViaConfig {
    fn default() -> Self {
        toml::from_str(include_str!("../../config/via.toml"))
            .expect("tracked Via configuration must be valid")
    }
}

impl ViaConfig {
    pub fn resolve_path(explicit: Option<&Path>, environment: Option<PathBuf>) -> PathBuf {
        explicit
            .map(Path::to_path_buf)
            .or(environment)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("config/via.toml"))
    }

    pub fn load(explicit: Option<&Path>) -> Result<Self, ConfigError> {
        let path = Self::resolve_path(explicit, std::env::var_os("VIA_CONFIG").map(PathBuf::from));
        let body = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        Self::from_toml(&body).map_err(|source| ConfigError::File {
            path,
            source: Box::new(source),
        })
    }

    pub fn from_toml(body: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(body)
            .map_err(|error| ConfigError::Invalid(error.message().to_owned()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn apply_environment(&mut self) -> Result<(), ConfigError> {
        let port = std::env::var("PORT").ok();
        let tdx = std::env::var("VIA_TDX_BASE_URL").ok();
        let valhalla = std::env::var("VIA_VALHALLA_URL").ok();
        self.apply_environment_values(port.as_deref(), tdx.as_deref(), valhalla.as_deref())
    }

    pub fn apply_environment_values(
        &mut self,
        port: Option<&str>,
        tdx: Option<&str>,
        valhalla: Option<&str>,
    ) -> Result<(), ConfigError> {
        if let Some(value) = port {
            self.server.port = super::parse_port(Some(value))?;
        }
        if let Some(value) = tdx {
            self.tdx.api_base_url = value.to_owned();
        }
        if let Some(value) = valhalla {
            self.valhalla.base_url = value.to_owned();
        }
        self.validate()
    }

    pub fn validate_environment(&self, environment: Environment) -> Result<(), ConfigError> {
        self.validate()?;
        if environment.is_production() {
            for (key, value) in [
                ("tdx.api_base_url", &self.tdx.api_base_url),
                ("tdx.token_url", &self.tdx.token_url),
                ("valhalla.base_url", &self.valhalla.base_url),
            ] {
                if super::parse_url(key, value)?.scheme() != "https" {
                    return Err(ConfigError::InsecureProductionUrl { setting: key });
                }
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        for (key, value) in [
            (
                "preprocessing.same_stop_transfer_seconds",
                self.preprocessing.same_stop_transfer_seconds as u64,
            ),
            ("server.port", self.server.port as u64),
            ("server.request_timeout_ms", self.server.request_timeout_ms),
            ("server.http_timeout_ms", self.server.http_timeout_ms),
            (
                "server.max_concurrent_requests",
                self.server.max_concurrent_requests as u64,
            ),
            ("server.max_body_bytes", self.server.max_body_bytes as u64),
            (
                "routing.max_static_corridors",
                self.routing.max_static_corridors as u64,
            ),
            ("routing.max_candidates", self.routing.max_candidates as u64),
            (
                "routing.max_departures_per_query",
                self.routing.max_departures_per_query as u64,
            ),
            (
                "routing.max_journey_duration_seconds",
                self.routing.max_journey_duration_seconds as u64,
            ),
            (
                "routing.access.max_transit_access_stops",
                self.routing.access.max_transit_access_stops as u64,
            ),
            (
                "routing.access.max_transit_egress_stops",
                self.routing.access.max_transit_egress_stops as u64,
            ),
            (
                "routing.access.max_walking_radius_metres",
                self.routing.access.max_walking_radius_metres as u64,
            ),
            (
                "routing.access.max_walking_seconds",
                self.routing.access.max_walking_seconds as u64,
            ),
            (
                "routing.ranking.metro_transfer_buffer_seconds",
                self.routing.ranking.metro_transfer_buffer_seconds as u64,
            ),
            (
                "routing.ranking.bus_transfer_buffer_seconds",
                self.routing.ranking.bus_transfer_buffer_seconds as u64,
            ),
            (
                "routing.ranking.near_duplicate_arrival_seconds",
                self.routing.ranking.near_duplicate_arrival_seconds as u64,
            ),
            (
                "routing.ranking.material_walking_difference_seconds",
                self.routing.ranking.material_walking_difference_seconds as u64,
            ),
            (
                "routing.ranking.max_sequence_only_option_delay_seconds",
                self.routing.ranking.max_sequence_only_option_delay_seconds as u64,
            ),
            (
                "routing.ranking.reliability_preference_window_seconds",
                self.routing.ranking.reliability_preference_window_seconds as u64,
            ),
            (
                "routing.ranking.result_limit",
                self.routing.ranking.result_limit as u64,
            ),
            (
                "bikeshare.max_pickup_stations_per_anchor",
                self.bikeshare.max_pickup_stations_per_anchor as u64,
            ),
            (
                "bikeshare.max_dropoff_stations_per_anchor",
                self.bikeshare.max_dropoff_stations_per_anchor as u64,
            ),
            (
                "bikeshare.max_stations_per_endpoint",
                self.bikeshare.max_stations_per_endpoint as u64,
            ),
            (
                "bikeshare.endpoint_radius_metres",
                self.bikeshare.endpoint_radius_metres as u64,
            ),
            (
                "bikeshare.max_endpoint_walk_seconds",
                self.bikeshare.max_endpoint_walk_seconds as u64,
            ),
            (
                "bikeshare.max_bike_duration_seconds",
                self.bikeshare.max_bike_duration_seconds as u64,
            ),
            (
                "bikeshare.max_bike_distance_metres",
                self.bikeshare.max_bike_distance_metres as u64,
            ),
            (
                "bikeshare.preprocessing.max_geometric_radius_metres",
                self.bikeshare.preprocessing.max_geometric_radius_metres as u64,
            ),
            (
                "bikeshare.preprocessing.max_walking_duration_seconds",
                self.bikeshare.preprocessing.max_walking_duration_seconds as u64,
            ),
            (
                "bikeshare.preprocessing.max_walking_distance_metres",
                self.bikeshare.preprocessing.max_walking_distance_metres as u64,
            ),
            (
                "bikeshare.preprocessing.max_geometric_candidates_per_boarding_point",
                self.bikeshare
                    .preprocessing
                    .max_geometric_candidates_per_boarding_point as u64,
            ),
            (
                "bikeshare.preprocessing.max_access_stations_per_boarding_point",
                self.bikeshare
                    .preprocessing
                    .max_access_stations_per_boarding_point as u64,
            ),
            (
                "bikeshare.preprocessing.max_matrix_concurrency",
                self.bikeshare.preprocessing.max_matrix_concurrency as u64,
            ),
            (
                "preprocessing.transfers.radius_metres",
                self.preprocessing.transfers.radius_metres as u64,
            ),
            (
                "preprocessing.transfers.max_duration_seconds",
                self.preprocessing.transfers.max_duration_seconds as u64,
            ),
            (
                "preprocessing.transfers.max_distance_metres",
                self.preprocessing.transfers.max_distance_metres as u64,
            ),
            (
                "preprocessing.transfers.max_outgoing",
                self.preprocessing.transfers.max_outgoing as u64,
            ),
            (
                "preprocessing.transfers.max_concurrency",
                self.preprocessing.transfers.max_concurrency as u64,
            ),
            (
                "preprocessing.transfers.source_group_size",
                self.preprocessing.transfers.source_group_size as u64,
            ),
            ("tdx.connect_timeout_ms", self.tdx.connect_timeout_ms),
            ("tdx.request_timeout_ms", self.tdx.request_timeout_ms),
            (
                "tdx.download_timeout_seconds",
                self.tdx.download_timeout_seconds,
            ),
            ("tdx.max_response_bytes", self.tdx.max_response_bytes as u64),
            (
                "tdx.minimum_token_lifetime_seconds",
                self.tdx.minimum_token_lifetime_seconds,
            ),
            (
                "tdx.token_refresh_margin_seconds",
                self.tdx.token_refresh_margin_seconds,
            ),
            (
                "tdx.bike_catalog_page_size",
                self.tdx.bike_catalog_page_size as u64,
            ),
            (
                "tdx.batch.bike_stations",
                self.tdx.batch.bike_stations as u64,
            ),
            ("tdx.batch.bus_stops", self.tdx.batch.bus_stops as u64),
            ("tdx.batch.metro_pairs", self.tdx.batch.metro_pairs as u64),
            (
                "tdx.batch.metro_response_limit",
                self.tdx.batch.metro_response_limit as u64,
            ),
            (
                "tdx.realtime.fresh_seconds",
                self.tdx.realtime.fresh_seconds as u64,
            ),
            (
                "tdx.realtime.degraded_seconds",
                self.tdx.realtime.degraded_seconds as u64,
            ),
            (
                "tdx.realtime.max_clock_skew_seconds",
                self.tdx.realtime.max_clock_skew_seconds as u64,
            ),
            (
                "tdx.realtime.cache_ttl_seconds",
                self.tdx.realtime.cache_ttl_seconds,
            ),
            (
                "tdx.realtime.cache_max_bytes",
                self.tdx.realtime.cache_max_bytes,
            ),
            (
                "tdx.bikeshare.cache_ttl_seconds",
                self.tdx.bikeshare.cache_ttl_seconds,
            ),
            (
                "tdx.bikeshare.cache_capacity",
                self.tdx.bikeshare.cache_capacity,
            ),
            (
                "tdx.bikeshare.max_concurrent_cities",
                self.tdx.bikeshare.max_concurrent_cities as u64,
            ),
            (
                "tdx.bikeshare.max_age_seconds",
                self.tdx.bikeshare.max_age_seconds as u64,
            ),
            (
                "tdx.bikeshare.max_clock_skew_seconds",
                self.tdx.bikeshare.max_clock_skew_seconds as u64,
            ),
            (
                "valhalla.request_timeout_ms",
                self.valhalla.request_timeout_ms,
            ),
            (
                "valhalla.build_request_timeout_ms",
                self.valhalla.build_request_timeout_ms,
            ),
            (
                "valhalla.max_concurrency",
                self.valhalla.max_concurrency as u64,
            ),
            ("valhalla.max_sources", self.valhalla.max_sources as u64),
            ("valhalla.max_targets", self.valhalla.max_targets as u64),
            (
                "valhalla.max_response_bytes",
                self.valhalla.max_response_bytes as u64,
            ),
        ] {
            if value == 0 {
                return Err(ConfigError::Invalid(format!(
                    "{key} must be greater than zero"
                )));
            }
            if (key.ends_with("_ms") || key.ends_with("_seconds")) && value > u64::from(u32::MAX) {
                return Err(ConfigError::Invalid(format!(
                    "{key} exceeds supported duration range"
                )));
            }
        }
        for (key, value) in [
            ("tdx.api_base_url", &self.tdx.api_base_url),
            ("tdx.token_url", &self.tdx.token_url),
            ("valhalla.base_url", &self.valhalla.base_url),
        ] {
            let url = super::parse_url(key, value)?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(ConfigError::Invalid(format!(
                    "{key} must be an HTTP(S) URL with a host and without credentials, query, or fragment"
                )));
            }
        }
        if self.tdx.realtime.degraded_seconds < self.tdx.realtime.fresh_seconds {
            return Err(ConfigError::Invalid(
                "tdx.realtime.degraded_seconds must be at least fresh_seconds".into(),
            ));
        }
        if self.tdx.realtime.cache_ttl_seconds >= u64::from(self.tdx.realtime.fresh_seconds) {
            return Err(ConfigError::Invalid(
                "tdx.realtime.cache_ttl_seconds must be less than fresh_seconds".into(),
            ));
        }
        for (key, value) in [
            (
                "routing.ranking.near_duplicate_arrival_seconds",
                self.routing.ranking.near_duplicate_arrival_seconds,
            ),
            (
                "routing.ranking.max_sequence_only_option_delay_seconds",
                self.routing.ranking.max_sequence_only_option_delay_seconds,
            ),
        ] {
            if value <= 0 || value > i64::from(u32::MAX) {
                return Err(ConfigError::Invalid(format!(
                    "{key} must be positive and fit supported duration range"
                )));
            }
        }
        let bike = self.bikeshare.preprocessing;
        if bike.max_geometric_candidates_per_boarding_point > self.valhalla.max_sources
            || bike.max_geometric_candidates_per_boarding_point > self.valhalla.max_targets
        {
            return Err(ConfigError::Invalid("bikeshare.preprocessing.max_geometric_candidates_per_boarding_point must fit both valhalla.max_sources and valhalla.max_targets".into()));
        }
        if self
            .valhalla
            .max_sources
            .checked_mul(self.valhalla.max_targets)
            .is_none()
        {
            return Err(ConfigError::Invalid(
                "valhalla matrix dimensions overflow".into(),
            ));
        }

        if bike.max_access_stations_per_boarding_point
            > bike.max_geometric_candidates_per_boarding_point
        {
            return Err(ConfigError::Invalid("bikeshare.preprocessing.max_access_stations_per_boarding_point must not exceed max_geometric_candidates_per_boarding_point".into()));
        }
        for (key, count) in [
            (
                "server.max_concurrent_requests",
                self.server.max_concurrent_requests,
            ),
            ("valhalla.max_concurrency", self.valhalla.max_concurrency),
        ] {
            if count > tokio::sync::Semaphore::MAX_PERMITS {
                return Err(ConfigError::Invalid(format!(
                    "{key} exceeds semaphore capacity"
                )));
            }
        }
        if !self
            .bikeshare
            .optimistic_walk_speed_metres_per_second
            .is_finite()
            || self.bikeshare.optimistic_walk_speed_metres_per_second <= 0.0
        {
            return Err(ConfigError::Invalid(
                "bikeshare.optimistic_walk_speed_metres_per_second must be finite".into(),
            ));
        }
        if !(1..=8).contains(&self.routing.max_concurrent_phase2_corridors) {
            return Err(ConfigError::Invalid(
                "routing.max_concurrent_phase2_corridors must be in 1..=8".into(),
            ));
        }
        if !(1..=32).contains(&self.routing.max_concurrent_phase2_states) {
            return Err(ConfigError::Invalid(
                "routing.max_concurrent_phase2_states must be in 1..=32".into(),
            ));
        }
        if !(1..=16).contains(&self.routing.max_concurrent_reconstruction_candidates) {
            return Err(ConfigError::Invalid(
                "routing.max_concurrent_reconstruction_candidates must be in 1..=16".into(),
            ));
        }
        Ok(())
    }

    pub fn bike_policy(&self) -> crate::routing::BikeMovementPolicy {
        crate::routing::BikeMovementPolicy {
            max_pickup_stations_per_anchor: self.bikeshare.max_pickup_stations_per_anchor,
            max_dropoff_stations_per_anchor: self.bikeshare.max_dropoff_stations_per_anchor,
            max_stations_per_endpoint: self.bikeshare.max_stations_per_endpoint,
            endpoint_radius_metres: self.bikeshare.endpoint_radius_metres,
            max_endpoint_walk_seconds: self.bikeshare.max_endpoint_walk_seconds,
            max_bike_duration_seconds: self.bikeshare.max_bike_duration_seconds,
            max_bike_distance_metres: self.bikeshare.max_bike_distance_metres,
            optimistic_walk_speed_metres_per_second: self
                .bikeshare
                .optimistic_walk_speed_metres_per_second,
            availability_max_age_seconds: self.tdx.bikeshare.max_age_seconds,
            availability_max_clock_skew_seconds: self.tdx.bikeshare.max_clock_skew_seconds,
        }
    }

    pub fn journey_limits(&self) -> JourneyPlanningLimits {
        JourneyPlanningLimits {
            bike: self.bike_policy(),
            ranking: self.routing.ranking.clone(),
            planner: self.planner_limits(),
            request_timeout: Duration::from_millis(self.server.request_timeout_ms),
            max_concurrent_requests: self.server.max_concurrent_requests,
            max_concurrent_reconstruction_candidates: self
                .routing
                .max_concurrent_reconstruction_candidates,
            max_transit_access_stops: self.routing.access.max_transit_access_stops,
            max_transit_egress_stops: self.routing.access.max_transit_egress_stops,
            max_walking_radius_metres: self.routing.access.max_walking_radius_metres,
            max_walking_seconds: self.routing.access.max_walking_seconds,
        }
    }

    pub fn planner_limits(&self) -> PlannerLimits {
        PlannerLimits {
            max_transfers: self.routing.max_transfers,
            max_static_corridors: self.routing.max_static_corridors,
            max_concurrent_phase2_states: self.routing.max_concurrent_phase2_states,
            max_concurrent_phase2_corridors: self.routing.max_concurrent_phase2_corridors,
            max_candidates: self.routing.max_candidates,
            max_departures_per_query: self.routing.max_departures_per_query,
            max_journey_duration_seconds: self.routing.max_journey_duration_seconds,
            metro_transfer_penalty_seconds: self.routing.ranking.metro_transfer_buffer_seconds,
            bus_transfer_penalty_seconds: self.routing.ranking.bus_transfer_buffer_seconds,
        }
    }
}

impl ValhallaSettings {
    pub fn provider_config(&self, build: bool) -> Result<ValhallaConfig, ConfigError> {
        Ok(ValhallaConfig {
            base_url: super::parse_url("valhalla.base_url", &self.base_url)?,
            request_timeout: Duration::from_millis(if build {
                self.build_request_timeout_ms
            } else {
                self.request_timeout_ms
            }),
            max_concurrency: self.max_concurrency,
            max_sources: self.max_sources,
            max_targets: self.max_targets,
            max_response_bytes: self.max_response_bytes,
        })
    }
}

impl Default for PlannerLimits {
    fn default() -> Self {
        crate::config::ViaConfig::default().planner_limits()
    }
}

impl Default for JourneyPlanningLimits {
    fn default() -> Self {
        crate::config::ViaConfig::default().journey_limits()
    }
}

impl Default for crate::routing::BikeMovementPolicy {
    fn default() -> Self {
        ViaConfig::default().bike_policy()
    }
}
