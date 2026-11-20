use serde::{Deserialize, Serialize};
macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_id!(BoardingPointId);
opaque_id!(BikeStationId);
opaque_id!(TransitRouteId);
opaque_id!(TransitPatternId);
opaque_id!(TransitTripId);
opaque_id!(ServiceCalendarId);
opaque_id!(EffectiveTripId);

const DIRECTIONAL_SERVICE_SEPARATOR: &str = "::direction=";

pub fn boarding_point_id(stop_uid: impl Into<String>) -> BoardingPointId {
    BoardingPointId::new(stop_uid)
}

pub fn bike_station_id(city: impl Into<String>, station_uid: impl Into<String>) -> BikeStationId {
    BikeStationId::new(format!("tdx:{}:{}", city.into(), station_uid.into()))
}

pub fn transit_service_id(sub_route_uid: impl Into<String>) -> TransitPatternId {
    TransitPatternId::new(sub_route_uid)
}

pub fn transit_service_id_for_direction(
    sub_route_uid: impl Into<String>,
    direction: u8,
) -> TransitPatternId {
    TransitPatternId::new(format!(
        "{}{DIRECTIONAL_SERVICE_SEPARATOR}{direction}",
        sub_route_uid.into()
    ))
}

pub fn tdx_sub_route_uid(pattern_id: &TransitPatternId) -> &str {
    pattern_id
        .as_str()
        .split_once(DIRECTIONAL_SERVICE_SEPARATOR)
        .map_or_else(|| pattern_id.as_str(), |(sub_route_uid, _)| sub_route_uid)
}
