//! Shared, transport-only TDX concerns.

mod client;

pub use client::{TdxClient, TdxClientError, TdxCredentials, TdxCredentialsError, TdxRawResponse};

const DIRECTIONAL_SERVICE_SEPARATOR: &str = "::direction=";

/// Construct the domain ID for a TDX stop or other boarding point.
pub fn boarding_point_id(stop_uid: impl Into<String>) -> crate::domain::ids::BoardingPointId {
    crate::domain::ids::BoardingPointId::new(stop_uid)
}

/// Construct the domain ID for a TDX sub-route service.
pub fn transit_service_id(
    sub_route_uid: impl Into<String>,
) -> crate::domain::ids::TransitServiceId {
    crate::domain::ids::TransitServiceId::new(sub_route_uid)
}

/// Construct a direction-sensitive TDX service ID.
pub fn transit_service_id_for_direction(
    sub_route_uid: impl Into<String>,
    direction: u8,
) -> crate::domain::ids::TransitServiceId {
    crate::domain::ids::TransitServiceId::new(format!(
        "{}{DIRECTIONAL_SERVICE_SEPARATOR}{direction}",
        sub_route_uid.into()
    ))
}

/// Return the base TDX sub-route UID represented by a possibly directional ID.
pub fn tdx_sub_route_uid(service_id: &crate::domain::ids::TransitServiceId) -> &str {
    service_id
        .as_str()
        .split_once(DIRECTIONAL_SERVICE_SEPARATOR)
        .map_or_else(|| service_id.as_str(), |(sub_route_uid, _)| sub_route_uid)
}
