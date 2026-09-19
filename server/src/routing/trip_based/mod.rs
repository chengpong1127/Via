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

use index::TripBasedIndex;

#[derive(Clone)]
pub struct TripBasedRouter {
    network: Arc<TransitNetwork>,
    index: Arc<TripBasedIndex>,
    departure_resolver: Arc<dyn DepartureResolver>,
    transfer_resolver: Arc<dyn TransferResolver>,
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
        }
    }
}
