//! Trip-Based transit routing.

mod boarding;
mod diagnostics;
mod dominance;
mod index;
mod search;
mod state;

use super::transfers::TransferResolver;
use crate::domain::{transit::TransitNetwork, transit_timing::DepartureResolver};
use std::sync::Arc;

pub use index::{Line, LinePosition, TripBasedIndex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TripBasedLimits {
    pub max_transfers: usize,
    pub max_candidates: usize,
    pub max_expansions: usize,
    pub max_departures_per_query: usize,
    pub max_journey_duration_seconds: u32,
}

impl Default for TripBasedLimits {
    fn default() -> Self {
        Self {
            max_transfers: 4,
            max_candidates: 16,
            max_expansions: 2_000,
            max_departures_per_query: 3,
            max_journey_duration_seconds: 4 * 60 * 60,
        }
    }
}

#[derive(Clone)]
pub struct TripBasedRouter {
    network: Arc<TransitNetwork>,
    index: Arc<TripBasedIndex>,
    departure_resolver: Arc<dyn DepartureResolver>,
    transfer_resolver: Arc<dyn TransferResolver>,
    limits: TripBasedLimits,
}

impl TripBasedRouter {
    pub fn new(
        network: Arc<TransitNetwork>,
        departure_resolver: Arc<dyn DepartureResolver>,
        transfer_resolver: Arc<dyn TransferResolver>,
    ) -> Self {
        let index = Arc::new(TripBasedIndex::new(&network));
        Self {
            network,
            index,
            departure_resolver,
            transfer_resolver,
            limits: TripBasedLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: TripBasedLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn index(&self) -> &TripBasedIndex {
        &self.index
    }

    pub fn network(&self) -> &Arc<TransitNetwork> {
        &self.network
    }

    pub fn limits(&self) -> &TripBasedLimits {
        &self.limits
    }
}
