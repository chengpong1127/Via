//! Diagnostics collected while reading and normalizing a GTFS archive.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::data::static_transfers::TransferStatistics;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GtfsBuildDiagnostics {
    #[serde(skip)]
    pub static_transfers: Option<TransferStatistics>,
    pub stops_loaded: usize,
    pub routes_loaded_by_mode: BTreeMap<String, usize>,
    pub patterns_loaded_by_mode: BTreeMap<String, usize>,
    pub trips_loaded: usize,
    pub scheduled_trips_loaded: usize,
    pub frequencies_loaded: usize,
    pub shape_rows_loaded: usize,
    pub invalid_timing_trips: usize,
    pub invalid_timing_trip_examples: Vec<String>,
    pub exact_static_timing_patterns: usize,
    pub derived_bus_timing_patterns: usize,
    pub geometric_fallback_patterns: usize,
    pub unresolved_bus_realtime_scope_patterns: usize,
    pub unresolved_bus_realtime_scope_examples: Vec<String>,
    pub gtfs_stop_ids_without_tdx_mapping: usize,
    pub unmapped_stop_id_examples: Vec<String>,
    pub gtfs_pattern_ids_without_tdx_mapping: usize,
    pub unmapped_pattern_id_examples: Vec<String>,
}
