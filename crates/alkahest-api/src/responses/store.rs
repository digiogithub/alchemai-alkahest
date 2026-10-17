//! In-memory store of finished responses, for `previous_response_id` and
//! `GET /v1/responses/{id}`.
//!
//! Responses live in memory only (lost on restart), expire after [`ResponseStore::ttl`] and
//! the oldest are evicted beyond [`ResponseStore::capacity`]. That is enough for agent loops
//! that chain turns within a session; durable storage is not part of ALK-SP-0005.R2.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;

/// A stored response.
#[derive(Debug, Clone)]
pub struct Stored {
    /// The conversation a follow-up continues from (chat messages, without instructions).
    pub conversation: Vec<Value>,
    /// The response object as returned to the client.
    pub response: Value,
    at: Instant,
}

/// The store. Register one in the state as `Arc<ResponseStore>` to override the
/// process-wide default.
#[derive(Debug)]
pub struct ResponseStore {
    entries: Mutex<HashMap<String, Stored>>,
    ttl: Duration,
    capacity: usize,
}

impl Default for ResponseStore {
    fn default() -> Self {
        Self::new(Duration::from_secs(3600), 1000)
    }
}

impl ResponseStore {
    /// A store keeping responses for `ttl`, at most `capacity` of them.
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            capacity: capacity.max(1),
        }
    }

    /// How long a response is kept.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// How many responses are kept at most.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn prune(&self, map: &mut HashMap<String, Stored>) {
        let ttl = self.ttl;
        map.retain(|_, s| s.at.elapsed() < ttl);
        while map.len() >= self.capacity {
            let Some(oldest) = map.iter().min_by_key(|(_, s)| s.at).map(|(k, _)| k.clone()) else {
                break;
            };
            map.remove(&oldest);
        }
    }

    /// Stores a finished response.
    pub fn put(&self, id: &str, conversation: Vec<Value>, response: Value) {
        let mut map = self.entries.lock();
        self.prune(&mut map);
        map.insert(
            id.to_string(),
            Stored {
                conversation,
                response,
                at: Instant::now(),
            },
        );
    }

    /// A stored response that has not expired.
    pub fn get(&self, id: &str) -> Option<Stored> {
        let map = self.entries.lock();
        map.get(id).filter(|s| s.at.elapsed() < self.ttl).cloned()
    }

    /// Removes a response; true when it existed.
    pub fn delete(&self, id: &str) -> bool {
        self.entries.lock().remove(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expiry_and_capacity() {
        let s = ResponseStore::new(Duration::from_millis(30), 2);
        s.put("a", vec![], json!(1));
        s.put("b", vec![], json!(2));
        s.put("c", vec![], json!(3));
        assert!(s.get("a").is_none(), "evicted as the oldest");
        assert_eq!(s.get("c").unwrap().response, json!(3));
        std::thread::sleep(Duration::from_millis(40));
        assert!(s.get("c").is_none(), "expired");
        assert!(!s.delete("zzz"));
    }
}
