use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::domain::transit_timing::DepartureError;

#[derive(Clone, Default)]
pub struct BusTimingMetrics {
    logical_departure_needs: Arc<AtomicU64>,
    realtime_stops_requested: Arc<AtomicU64>,
    realtime_batches: Arc<AtomicU64>,
    realtime_http_requests: Arc<AtomicU64>,
    realtime_rows_received: Arc<AtomicU64>,
    realtime_rows_relevant: Arc<AtomicU64>,
    upstream_failures: Arc<AtomicU64>,
    rate_limited_requests: Arc<AtomicU64>,
}

impl BusTimingMetrics {
    pub(crate) fn record_departure_batch(
        &self,
        logical_needs: usize,
        relevant_stops: usize,
        batch_count: usize,
    ) {
        self.logical_departure_needs
            .fetch_add(logical_needs as u64, Ordering::Relaxed);
        self.realtime_stops_requested
            .fetch_add(relevant_stops as u64, Ordering::Relaxed);
        self.realtime_batches
            .fetch_add(batch_count as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_departure_upstream_request(&self) {
        self.realtime_http_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_departure_upstream_result(&self, error: Option<&DepartureError>) {
        if let Some(error) = error {
            self.upstream_failures.fetch_add(1, Ordering::Relaxed);
            if matches!(error, DepartureError::RateLimited) {
                self.rate_limited_requests.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn record_snapshot_rows(&self, received: usize, relevant: usize) {
        self.realtime_rows_received
            .fetch_add(received as u64, Ordering::Relaxed);
        self.realtime_rows_relevant
            .fetch_add(relevant as u64, Ordering::Relaxed);
    }

    pub fn trace(&self) -> String {
        format!(
            "BUS TIMING\n  logical needs={}\n  realtime stops={}\n  realtime batches={}\n  HTTP requests={}\n  rows received={}\n  rows relevant={}",
            self.logical_departure_needs.load(Ordering::Relaxed),
            self.realtime_stops_requested.load(Ordering::Relaxed),
            self.realtime_batches.load(Ordering::Relaxed),
            self.realtime_http_requests.load(Ordering::Relaxed),
            self.realtime_rows_received.load(Ordering::Relaxed),
            self.realtime_rows_relevant.load(Ordering::Relaxed),
        )
    }

    pub fn prometheus(&self) -> String {
        format!(
            "# TYPE via_bus_timing_logical_departure_needs_total counter\nvia_bus_timing_logical_departure_needs_total {}\n\
# TYPE via_bus_timing_realtime_stops_requested_total counter\nvia_bus_timing_realtime_stops_requested_total {}\n\
# TYPE via_bus_timing_realtime_batches_total counter\nvia_bus_timing_realtime_batches_total {}\n\
# TYPE via_bus_timing_realtime_http_requests_total counter\nvia_bus_timing_realtime_http_requests_total {}\n\
# TYPE via_bus_timing_realtime_rows_received_total counter\nvia_bus_timing_realtime_rows_received_total {}\n\
# TYPE via_bus_timing_realtime_rows_relevant_total counter\nvia_bus_timing_realtime_rows_relevant_total {}\n\
# TYPE via_bus_timing_upstream_failures_total counter\nvia_bus_timing_upstream_failures_total {}\n\
# TYPE via_bus_timing_rate_limited_requests_total counter\nvia_bus_timing_rate_limited_requests_total {}\n",
            self.logical_departure_needs.load(Ordering::Relaxed),
            self.realtime_stops_requested.load(Ordering::Relaxed),
            self.realtime_batches.load(Ordering::Relaxed),
            self.realtime_http_requests.load(Ordering::Relaxed),
            self.realtime_rows_received.load(Ordering::Relaxed),
            self.realtime_rows_relevant.load(Ordering::Relaxed),
            self.upstream_failures.load(Ordering::Relaxed),
            self.rate_limited_requests.load(Ordering::Relaxed),
        )
    }
}
