//! TDX identity metadata kept beside, rather than inside, the Via transit network.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::domain::ids::{TransitPatternId, TransitTripId};
use crate::domain::transit::{TransitMode, TransitNetwork};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TdxProviderBindings {
    pub bus_patterns: HashMap<TransitPatternId, BusPatternBinding>,
    pub metro_patterns: HashMap<TransitPatternId, MetroPatternBinding>,
    pub tra_trips: HashMap<TransitTripId, TraTripBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusPatternBinding {
    pub tdx_service_id: String,
    pub realtime_scope: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetroPatternBinding {
    pub line_id: String,
    pub direction: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
