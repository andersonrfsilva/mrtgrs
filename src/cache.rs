use serde::Serialize;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

pub struct ResponseCache {
    entries: HashMap<String, (Instant, Arc<[u8]>)>,
    ttl: Duration,
    max_size: usize,
    max_bytes: usize,
    used_bytes: usize,
}
#[derive(Serialize)]
pub struct CacheStats {
    total_entries: usize,
    live_entries: usize,
    ttl: u64,
    max_size: usize,
    used_bytes: usize,
    max_bytes: usize,
}
impl ResponseCache {
    pub fn new(ttl: u64, max_size: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            ttl: Duration::from_secs(ttl),
            max_size,
            max_bytes,
            used_bytes: 0,
        }
    }
    fn remove(&mut self, key: &str) {
        if let Some((_, value)) = self.entries.remove(key) {
            self.used_bytes -= value.len();
        }
    }
    pub fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        if self
            .entries
            .get(key)
            .is_some_and(|(expiry, _)| *expiry <= Instant::now())
        {
            self.remove(key);
        }
        self.entries.get(key).map(|(_, value)| Arc::clone(value))
    }
    pub fn put(&mut self, key: String, value: Arc<[u8]>) {
        if value.len() > self.max_bytes || self.max_size == 0 {
            return;
        }
        self.remove(&key);
        let now = Instant::now();
        if self.entries.len() >= self.max_size || self.used_bytes + value.len() > self.max_bytes {
            let expired: Vec<String> = self
                .entries
                .iter()
                .filter(|(_, (expiry, _))| *expiry <= now)
                .map(|(key, _)| key.clone())
                .collect();
            for key in expired {
                self.remove(&key);
            }
        }
        while self.entries.len() >= self.max_size || self.used_bytes + value.len() > self.max_bytes
        {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (expiry, _))| *expiry)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.remove(&oldest);
        }
        self.used_bytes += value.len();
        self.entries.insert(key, (now + self.ttl, value));
    }
    pub fn stats(&self) -> CacheStats {
        let now = Instant::now();
        CacheStats {
            total_entries: self.entries.len(),
            live_entries: self
                .entries
                .values()
                .filter(|(expiry, _)| *expiry > now)
                .count(),
            ttl: self.ttl.as_secs(),
            max_size: self.max_size,
            used_bytes: self.used_bytes,
            max_bytes: self.max_bytes,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hit_miss_expiry() {
        let mut c = ResponseCache::new(0, 2, 10);
        c.put("a".into(), Arc::from(&b"a"[..]));
        assert!(c.get("a").is_none());
        assert!(c.get("missing").is_none());
        let mut c = ResponseCache::new(60, 2, 10);
        c.put("a".into(), Arc::from(&b"a"[..]));
        assert!(c.get("a").is_some());
    }
    #[test]
    fn size_and_byte_eviction() {
        let mut c = ResponseCache::new(60, 1, 4);
        c.put("a".into(), Arc::from(&b"aa"[..]));
        c.put("b".into(), Arc::from(&b"bb"[..]));
        assert!(c.get("a").is_none());
        let mut c = ResponseCache::new(60, 3, 3);
        c.put("a".into(), Arc::from(&b"aa"[..]));
        c.put("b".into(), Arc::from(&b"bb"[..]));
        assert!(c.get("a").is_none());
        assert!(c.get("b").is_some());
    }
}
