use std::collections::HashMap;

use serde::Deserialize;

use crate::domain::ids::TransitPatternId;
use crate::domain::ids::tdx_sub_route_uid;
use crate::providers::tdx_bindings::TdxProviderBindings;

pub const BUS_ROUTE_CITIES: &[(&str, &str)] = &[
    ("ChanghuaCounty", "changhua-county"),
    ("Chiayi", "chiayi"),
    ("ChiayiCounty", "chiayi-county"),
    ("Hsinchu", "hsinchu"),
    ("HsinchuCounty", "hsinchu-county"),
    ("HualienCounty", "hualien-county"),
    ("Kaohsiung", "kaohsiung"),
    ("Keelung", "keelung"),
    ("KinmenCounty", "kinmen-county"),
    ("LienchiangCounty", "lienchiang-county"),
    ("MiaoliCounty", "miaoli-county"),
    ("NantouCounty", "nantou-county"),
    ("NewTaipei", "new-taipei"),
    ("PenghuCounty", "penghu-county"),
    ("PingtungCounty", "pingtung-county"),
    ("Taichung", "taichung"),
    ("Tainan", "tainan"),
    ("Taipei", "taipei"),
    ("TaitungCounty", "taitung-county"),
    ("Taoyuan", "taoyuan"),
    ("YilanCounty", "yilan-county"),
    ("YunlinCounty", "yunlin-county"),
];

pub const BUS_ROUTE_FIELDS: &str = "RouteUID,SubRoutes";

#[derive(Clone, Debug, Deserialize)]
pub struct BusRoute {
    #[serde(rename = "RouteUID")]
    pub route_uid: String,
    #[serde(rename = "SubRoutes", default)]
    pub sub_routes: Vec<BusSubRoute>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BusSubRoute {
    #[serde(rename = "SubRouteUID")]
    pub sub_route_uid: String,
}

pub fn attach_parent_routes(bindings: &mut TdxProviderBindings, routes: &[BusRoute]) -> usize {
    let parent_by_sub_route = routes
        .iter()
        .flat_map(|route| {
            route
                .sub_routes
                .iter()
                .filter(move |sub_route| sub_route.sub_route_uid != route.route_uid)
                .map(move |sub_route| (sub_route.sub_route_uid.as_str(), route.route_uid.as_str()))
        })
        .collect::<HashMap<_, _>>();
    let mut attached = 0;
    for binding in bindings.bus_patterns.values_mut() {
        let service = TransitPatternId::new(binding.tdx_service_id.clone());
        binding.tdx_route_uid = parent_by_sub_route
            .get(tdx_sub_route_uid(&service))
            .map(|parent| (*parent).to_owned());
        attached += usize::from(binding.tdx_route_uid.is_some());
    }
    attached
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::transit_service_id_for_direction;
    use crate::providers::tdx_bindings::BusPatternBinding;

    #[test]
    fn routes_attach_tdx_parent_routes_to_bus_bindings() {
        let routes: Vec<BusRoute> = serde_json::from_str(
            r#"[
                {"RouteUID":"NWT16517","SubRoutes":[
                    {"SubRouteUID":"NWT157552","Direction":0},
                    {"SubRouteUID":"NWT157552","Direction":1}]},
                {"RouteUID":"TAO1","SubRoutes":[{"SubRouteUID":"TAO1"}]}
            ]"#,
        )
        .unwrap();
        let mut bindings = TdxProviderBindings::default();
        for (pattern, sub_route, direction) in [
            ("p704", "NWT157552", 0),
            ("pTao", "TAO1", 1),
            ("pOther", "NWT99990", 0),
        ] {
            bindings.bus_patterns.insert(
                TransitPatternId::new(pattern),
                BusPatternBinding {
                    tdx_service_id: transit_service_id_for_direction(sub_route, direction)
                        .as_str()
                        .to_owned(),
                    realtime_scope: "NewTaipei".to_owned(),
                    tdx_route_uid: None,
                },
            );
        }

        assert_eq!(attach_parent_routes(&mut bindings, &routes), 1);
        let parent = |pattern: &str| {
            bindings.bus_patterns[&TransitPatternId::new(pattern)]
                .tdx_route_uid
                .clone()
        };
        assert_eq!(parent("p704").as_deref(), Some("NWT16517"));
        assert_eq!(parent("pTao"), None);
        assert_eq!(parent("pOther"), None);
    }
}
