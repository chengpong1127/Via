use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

#[derive(Default)]
pub(crate) struct RouteMeasurement {
    pub(crate) waits_us: Mutex<Vec<u64>>,
    pub(crate) wait_attempts: AtomicUsize,
    pub(crate) waiting: AtomicUsize,
    pub(crate) max_waiting: AtomicUsize,
    pub(crate) attempts: AtomicUsize,
    pub(crate) responses: AtomicUsize,
    pub(crate) success: AtomicUsize,
    pub(crate) incomplete: AtomicUsize,
    pub(crate) active: AtomicUsize,
    pub(crate) max_active: AtomicUsize,
    pub(crate) http_us: AtomicUsize,
}
impl RouteMeasurement {
    pub(crate) fn reset(&self) {
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        assert_eq!(self.waiting.load(Ordering::SeqCst), 0);
        self.waits_us.lock().unwrap().clear();
        for value in [
            &self.wait_attempts,
            &self.max_waiting,
            &self.attempts,
            &self.responses,
            &self.success,
            &self.incomplete,
            &self.max_active,
            &self.http_us,
        ] {
            value.store(0, Ordering::SeqCst);
        }
    }
    pub(super) fn wait(&self) -> WaitGuard<'_> {
        self.wait_attempts.fetch_add(1, Ordering::SeqCst);
        let count = self.waiting.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_waiting.fetch_max(count, Ordering::SeqCst);
        WaitGuard {
            stats: self,
            started: Instant::now(),
        }
    }
    pub(super) fn http(&self) -> HttpGuard<'_> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(count, Ordering::SeqCst);
        HttpGuard {
            stats: self,
            started: Instant::now(),
            completed: false,
        }
    }
}
pub(super) struct WaitGuard<'a> {
    stats: &'a RouteMeasurement,
    started: Instant,
}
impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        self.stats.waiting.fetch_sub(1, Ordering::SeqCst);
        self.stats
            .waits_us
            .lock()
            .unwrap()
            .push(self.started.elapsed().as_micros().min(u64::MAX as u128) as u64);
    }
}
pub(super) struct HttpGuard<'a> {
    stats: &'a RouteMeasurement,
    started: Instant,
    completed: bool,
}
impl HttpGuard<'_> {
    pub(super) fn finish(&mut self, success: bool) {
        self.completed = true;
        self.stats.responses.fetch_add(1, Ordering::SeqCst);
        if success {
            self.stats.success.fetch_add(1, Ordering::SeqCst);
        }
    }
}
impl Drop for HttpGuard<'_> {
    fn drop(&mut self) {
        self.stats.active.fetch_sub(1, Ordering::SeqCst);
        self.stats.http_us.fetch_add(
            self.started.elapsed().as_micros().min(usize::MAX as u128) as usize,
            Ordering::SeqCst,
        );
        if !self.completed {
            self.stats.incomplete.fetch_add(1, Ordering::SeqCst);
        }
    }
}
