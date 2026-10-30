use std::{
    collections::HashMap,
    future::Future,
    hash::Hash,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::OnceCell;

pub(in crate::routing) struct RequestCache<K, V> {
    entries: Mutex<HashMap<K, Arc<OnceCell<V>>>>,
    ready_hits: AtomicUsize,
    inflight_hits: AtomicUsize,
    misses: AtomicUsize,
    initializations: AtomicUsize,
    completed_entries: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::routing) struct CacheStats {
    pub ready_hits: usize,
    pub inflight_hits: usize,
    pub misses: usize,
    pub initializations: usize,
    pub completed_entries: usize,
}

impl<K, V> Default for RequestCache<K, V> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ready_hits: AtomicUsize::new(0),
            inflight_hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
            initializations: AtomicUsize::new(0),
            completed_entries: AtomicUsize::new(0),
        }
    }
}

impl<K: Eq + Hash, V: Clone> RequestCache<K, V> {
    pub async fn get_or_init<F, Fut>(&self, key: K, init: F) -> V
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = V>,
    {
        let cell = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cell) = entries.get(&key) {
                if cell.initialized() {
                    self.ready_hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.inflight_hits.fetch_add(1, Ordering::Relaxed);
                }
                cell.clone()
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
                let cell = Arc::new(OnceCell::new());
                entries.insert(key, cell.clone());
                cell
            }
        };
        cell.get_or_init(|| async {
            self.initializations.fetch_add(1, Ordering::Relaxed);
            let result = init().await;
            self.completed_entries.fetch_add(1, Ordering::Relaxed);
            result
        })
        .await
        .clone()
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            ready_hits: self.ready_hits.load(Ordering::Relaxed),
            inflight_hits: self.inflight_hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            initializations: self.initializations.load(Ordering::Relaxed),
            completed_entries: self.completed_entries.load(Ordering::Relaxed),
        }
    }
}
