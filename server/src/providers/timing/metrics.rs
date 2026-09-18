use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::domain::transit_timing::DepartureError;

#[derive(Clone, Default)]
pub struct BusTimingMetrics {
    logical_departure_needs: Arc<AtomicU64>,
    deduplicated_departure_needs: Arc<AtomicU64>,
    departure_batches: Arc<AtomicU64>,
    departure_upstream_requests: Arc<AtomicU64>,
    upstream_failures: Arc<AtomicU64>,
    rate_limited_requests: Arc<AtomicU64>,
}

impl BusTimingMetrics {
    pub(crate) fn record_departure_batch(
        &self,
        logical_needs: usize,
        unique_needs: usize,
        batch_count: usize,
    ) {
        self.logical_departure_needs
            .fetch_add(logical_needs as u64, Ordering::Relaxed);
        self.deduplicated_departure_needs
            .fetch_add(unique_needs as u64, Ordering::Relaxed);
        self.departure_batches
            .fetch_add(batch_count as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_departure_upstream_request(&self) {
        self.departure_upstream_requests
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_departure_upstream_result(&self, error: Option<&DepartureError>) {
        if let Some(error) = error {
            self.upstream_failures.fetch_add(1, Ordering::Relaxed);
            if matches!(error, DepartureError::RateLimited) {
                self.rate_limited_requests.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn trace(&self) -> String {
        format!(
            "BUS TIMING\n  logical needs={}\n  unique needs={}\n  upstream batches={}\n  upstream calls={}",
            self.logical_departure_needs.load(Ordering::Relaxed),
            self.deduplicated_departure_needs.load(Ordering::Relaxed),
            self.departure_batches.load(Ordering::Relaxed),
            self.departure_upstream_requests.load(Ordering::Relaxed),
        )
    }

    pub fn prometheus(&self) -> String {
        format!(
            "# TYPE via_bus_timing_logical_departure_needs_total counter\nvia_bus_timing_logical_departure_needs_total {}\n\
# TYPE via_bus_timing_deduplicated_departure_needs_total counter\nvia_bus_timing_deduplicated_departure_needs_total {}\n\
# TYPE via_bus_timing_departure_batches_total counter\nvia_bus_timing_departure_batches_total {}\n\
# TYPE via_bus_timing_upstream_requests_total counter\nvia_bus_timing_upstream_requests_total {}\n\
# TYPE via_bus_timing_upstream_failures_total counter\nvia_bus_timing_upstream_failures_total {}\n\
# TYPE via_bus_timing_rate_limited_requests_total counter\nvia_bus_timing_rate_limited_requests_total {}\n",
            self.logical_departure_needs.load(Ordering::Relaxed),
            self.deduplicated_departure_needs.load(Ordering::Relaxed),
            self.departure_batches.load(Ordering::Relaxed),
            self.departure_upstream_requests.load(Ordering::Relaxed),
            self.upstream_failures.load(Ordering::Relaxed),
            self.rate_limited_requests.load(Ordering::Relaxed),
        )
    }
}
