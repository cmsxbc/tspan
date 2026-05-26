use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

const TRACKER_STALE_SECS: u64 = 86400; // 24 hours

#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub session_id: i64,
    pub start_time: i64,
    pub client_id: String,
    pub inserted_at: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct Tracker {
    inner: Arc<Mutex<HashMap<u32, SessionMeta>>>,
}

impl Tracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn insert(&self, pid: u32, session_id: i64, start_time: i64, client_id: String) {
        let mut map = self.inner.lock();
        let now = Instant::now();
        // Evict stale entries to prevent unbounded growth from long-lived daemons
        let stale: Vec<u32> = map
            .iter()
            .filter(|(_, meta)| now.duration_since(meta.inserted_at).as_secs() > TRACKER_STALE_SECS)
            .map(|(&pid, _)| pid)
            .collect();
        for pid in stale {
            map.remove(&pid);
        }
        map.insert(
            pid,
            SessionMeta {
                session_id,
                start_time,
                client_id,
                inserted_at: now,
            },
        );
    }

    pub fn remove(&self, pid: u32) -> Option<SessionMeta> {
        let mut map = self.inner.lock();
        map.remove(&pid)
    }

    #[allow(dead_code)]
    pub fn get(&self, pid: u32) -> Option<SessionMeta> {
        let map = self.inner.lock();
        map.get(&pid).cloned()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        let map = self.inner.lock();
        map.len()
    }

    pub fn drain(&self) -> Vec<(u32, SessionMeta)> {
        let mut map = self.inner.lock();
        std::mem::take(&mut *map).into_iter().collect()
    }
}
