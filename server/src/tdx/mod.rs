mod client;

pub use client::{
    DEFAULT_TDX_API_BASE_URL, TdxClient, TdxClientError, TdxCredentials, TdxCredentialsError,
};

const DIRECTIONAL_SERVICE_SEPARATOR: &str = "::direction=";

pub fn boarding_point_id(stop_uid: impl Into<String>) -> crate::domain::ids::BoardingPointId {
    crate::domain::ids::BoardingPointId::new(stop_uid)
}

pub fn bike_station_id(
    city: impl Into<String>,
    station_uid: impl Into<String>,
) -> crate::domain::ids::BikeStationId {
    crate::domain::ids::BikeStationId::new(format!("tdx:{}:{}", city.into(), station_uid.into()))
}

pub fn transit_service_id(
    sub_route_uid: impl Into<String>,
) -> crate::domain::ids::TransitPatternId {
    crate::domain::ids::TransitPatternId::new(sub_route_uid)
}

pub fn transit_service_id_for_direction(
    sub_route_uid: impl Into<String>,
    direction: u8,
) -> crate::domain::ids::TransitPatternId {
    crate::domain::ids::TransitPatternId::new(format!(
        "{}{DIRECTIONAL_SERVICE_SEPARATOR}{direction}",
        sub_route_uid.into()
    ))
}

pub fn tdx_sub_route_uid(pattern_id: &crate::domain::ids::TransitPatternId) -> &str {
    pattern_id
        .as_str()
        .split_once(DIRECTIONAL_SERVICE_SEPARATOR)
        .map_or_else(|| pattern_id.as_str(), |(sub_route_uid, _)| sub_route_uid)
}
