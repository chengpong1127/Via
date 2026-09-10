use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Process-local operational counters. They intentionally contain only service
/// behavior, never Planning Request fields, Journey contents, or coordinates.
#[derive(Clone, Default)]
pub struct OperationalMetrics {
    inner: Arc<MetricsInner>,
}

#[derive(Default)]
struct MetricsInner {
    planning_requests_total: AtomicU64,
    planning_failures_total: AtomicU64,
    planning_empty_total: AtomicU64,
    planning_options_total: AtomicU64,
}

impl OperationalMetrics {
    pub fn record_planning_success(&self, options: usize) {
        self.inner
            .planning_requests_total
            .fetch_add(1, Ordering::Relaxed);
        if options == 0 {
            self.inner
                .planning_empty_total
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner
                .planning_options_total
                .fetch_add(options as u64, Ordering::Relaxed);
        }
    }

    pub fn record_planning_failure(&self) {
        self.inner
            .planning_requests_total
            .fetch_add(1, Ordering::Relaxed);
        self.inner
            .planning_failures_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn prometheus(&self) -> String {
        format!(
            "# TYPE via_planning_requests_total counter\nvia_planning_requests_total {}\n\
# TYPE via_planning_failures_total counter\nvia_planning_failures_total {}\n\
# TYPE via_planning_empty_total counter\nvia_planning_empty_total {}\n\
# TYPE via_planning_options_total counter\nvia_planning_options_total {}\n",
            self.inner.planning_requests_total.load(Ordering::Relaxed),
            self.inner.planning_failures_total.load(Ordering::Relaxed),
            self.inner.planning_empty_total.load(Ordering::Relaxed),
            self.inner.planning_options_total.load(Ordering::Relaxed),
        )
    }
}
