use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub session_id: i64,
    pub start_time: i64,
    #[allow(dead_code)]
    pub command: String,
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

    pub fn insert(&self, pid: u32, session_id: i64, start_time: i64, command: String) {
        let mut map = self.inner.lock();
        map.insert(pid, SessionMeta { session_id, start_time, command });
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
