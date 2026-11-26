use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::domain::ids::{TransitPatternId, TransitTripId};
use crate::domain::transit::network::{TransitMode, TransitNetwork};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TdxProviderBindings {
    pub bus_patterns: HashMap<TransitPatternId, BusPatternBinding>,
    pub metro_patterns: HashMap<TransitPatternId, MetroPatternBinding>,
    pub tra_trips: HashMap<TransitTripId, TraTripBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusPatternBinding {
    pub tdx_service_id: String,
    pub realtime_scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tdx_route_uid: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headway_seconds_by_period: Vec<Option<u32>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetroPatternBinding {
    pub line_id: String,
    pub direction: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraTripBinding {
    pub train_number: String,
}

impl TdxProviderBindings {
    pub fn validate(&self, network: &TransitNetwork) -> Result<(), String> {
        let patterns_by_id = network
            .patterns
            .iter()
            .map(|pattern| (&pattern.id, pattern))
            .collect::<HashMap<_, _>>();
        let routes_by_id = network
            .routes
            .iter()
            .map(|route| (&route.id, route))
            .collect::<HashMap<_, _>>();
        let trips_by_id = network
            .trips
            .iter()
            .map(|trip| (&trip.id, trip))
            .collect::<HashMap<_, _>>();
        for (id, binding) in &self.bus_patterns {
            let pattern = patterns_by_id
                .get(id)
                .copied()
                .ok_or_else(|| format!("unknown Bus pattern {}", id.as_str()))?;
            if routes_by_id
                .get(&pattern.route_id)
                .is_none_or(|route| route.mode != TransitMode::Bus)
                || binding.tdx_service_id.trim().is_empty()
                || binding.realtime_scope.trim().is_empty()
                || !(binding.headway_seconds_by_period.is_empty()
                    || binding.headway_seconds_by_period.len()
                        == crate::domain::transit::service::ServicePeriod::ALL.len())
            {
                return Err(format!("invalid Bus binding for {}", id.as_str()));
            }
        }
        for (id, binding) in &self.metro_patterns {
            let pattern = patterns_by_id
                .get(id)
                .copied()
                .ok_or_else(|| format!("unknown Metro pattern {}", id.as_str()))?;
            if routes_by_id
                .get(&pattern.route_id)
                .is_none_or(|route| route.mode != TransitMode::Metro)
                || binding.line_id.trim().is_empty()
                || binding.direction > 1
            {
                return Err(format!("invalid Metro binding for {}", id.as_str()));
            }
        }
        for (id, binding) in &self.tra_trips {
            let trip = trips_by_id
                .get(id)
                .copied()
                .ok_or_else(|| format!("unknown TRA trip {}", id.as_str()))?;
            let pattern = patterns_by_id
                .get(&trip.pattern_id)
                .copied()
                .ok_or_else(|| format!("unknown pattern for TRA trip {}", id.as_str()))?;
            if routes_by_id
                .get(&pattern.route_id)
                .is_none_or(|route| route.mode != TransitMode::Rail)
                || binding.train_number.trim().is_empty()
            {
                return Err(format!("invalid TRA binding for {}", id.as_str()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ids::{BoardingPointId, TransitRouteId},
        transit::network::{SegmentProfile, TransitPattern, TransitRoute},
    };

    fn bound(headways: Vec<Option<u32>>) -> (TdxProviderBindings, TransitNetwork) {
        let id = TransitPatternId::new("p");
        let network = TransitNetwork::new(
            time::Time::from_hms(4, 0, 0).unwrap(),
            Vec::new(),
            vec![TransitPattern::new(
                id.clone(),
                TransitRouteId::new("r"),
                Some(0),
                "T",
                vec![BoardingPointId::new("a"), BoardingPointId::new("b")],
                SegmentProfile {
                    segments: Vec::new(),
                },
            )],
            Vec::new(),
        )
        .with_routes(vec![TransitRoute {
            id: TransitRouteId::new("r"),
            mode: TransitMode::Bus,
            label: "r".into(),
        }]);
        let bindings = TdxProviderBindings {
            bus_patterns: HashMap::from([(
                id,
                BusPatternBinding {
                    tdx_service_id: "r".into(),
                    realtime_scope: "Taipei".into(),
                    tdx_route_uid: None,
                    headway_seconds_by_period: headways,
                },
            )]),
            ..TdxProviderBindings::default()
        };
        (bindings, network)
    }

    #[test]
    fn headway_vectors_must_cover_every_service_period() {
        let periods = crate::domain::transit::service::ServicePeriod::ALL.len();
        for headways in [Vec::new(), vec![Some(600); periods]] {
            let (bindings, network) = bound(headways);
            assert!(bindings.validate(&network).is_ok());
        }
        let (bindings, network) = bound(vec![Some(600); periods - 1]);
        assert!(bindings.validate(&network).is_err());
    }
}
