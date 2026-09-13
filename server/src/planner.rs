//! Compatibility exports for the pre-routing module path.
//!
//! The implementation lives under [`crate::routing::engine`]. Keeping this
//! facade avoids breaking existing crate users while the routing boundary is
//! migrated.

pub use crate::routing::engine::*;
pub use crate::routing::engine::{
    bike_share_expansion, candidate_discovery, multimodal_graph, search, transit_timing,
};
