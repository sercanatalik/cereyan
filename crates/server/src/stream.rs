//! Server-sent events: a ring buffer of the last changes with sequence
//! numbers and a broadcast channel for live subscribers.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::broadcast;

pub const RING_SIZE: usize = 65_536;

/// Sent once after a database reset, with its scope; clients reload everything.
pub const DATABASE_RESET: &str = "database.reset";

#[derive(Clone, Debug)]
pub struct StreamEvent {
    pub seq: u64,
    pub kind: &'static str,
    /// Entity key used for coalescing (run id, task run id, flow id).
    pub key: String,
    pub data: Value,
}

pub struct Broadcaster {
    ring: Mutex<VecDeque<Arc<StreamEvent>>>,
    next_seq: AtomicU64,
    tx: broadcast::Sender<Arc<StreamEvent>>,
}

impl Default for Broadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl Broadcaster {
    pub fn new() -> Broadcaster {
        let (tx, _) = broadcast::channel(RING_SIZE * 2);
        Broadcaster {
            ring: Mutex::new(VecDeque::with_capacity(RING_SIZE)),
            next_seq: AtomicU64::new(1),
            tx,
        }
    }

    pub fn publish(&self, kind: &'static str, key: impl Into<String>, data: Value) -> u64 {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let event = Arc::new(StreamEvent {
            seq,
            kind,
            key: key.into(),
            data,
        });
        {
            let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
            if ring.len() >= RING_SIZE {
                ring.pop_front();
            }
            ring.push_back(event.clone());
        }
        let _ = self.tx.send(event);
        seq
    }

    pub fn latest_seq(&self) -> u64 {
        self.next_seq.load(Ordering::SeqCst) - 1
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<StreamEvent>> {
        self.tx.subscribe()
    }

    /// Events after `since`, or None when `since` is older than the ring.
    ///
    /// Returns `None` when `since` is so old that the ring has wrapped around
    /// and the requested events are gone. Specifically, if `since < oldest - 1`,
    /// the events are no longer in the ring. The boundary case `since == oldest - 1`
    /// returns the oldest event (since `seq > since` includes it).
    pub fn replay(&self, since: u64) -> Option<Vec<Arc<StreamEvent>>> {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let oldest = ring.front().map(|e| e.seq);
        match oldest {
            None => Some(Vec::new()),
            // `since < oldest - 1` is equivalent to `since + 1 < oldest` but
            // more readable: the requested events are gone.
            Some(oldest) if since < oldest.saturating_sub(1) => None,
            Some(_) => Some(ring.iter().filter(|e| e.seq > since).cloned().collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_and_resync() {
        let b = Broadcaster::new();
        assert_eq!(b.replay(0).unwrap().len(), 0);
        for i in 0..(RING_SIZE + 10) {
            b.publish("run.updated", i.to_string(), serde_json::json!({"i": i}));
        }
        // Newer than the ring: only the tail comes back.
        let latest = b.latest_seq();
        assert_eq!(b.replay(latest - 5).unwrap().len(), 5);
        // Older than the ring holds: resync.
        assert!(b.replay(3).is_none());
        // Exactly the oldest buffered position is still replayable.
        let oldest = latest - RING_SIZE as u64 + 1;
        assert_eq!(b.replay(oldest - 1).unwrap().len(), RING_SIZE);
    }

    #[test]
    fn replay_boundary_case() {
        let b = Broadcaster::new();
        // Publish 5 events with seq 1..=5.
        for i in 1..=5 {
            b.publish("run.updated", i.to_string(), serde_json::json!({"i": i}));
        }
        // since = 0: all events are after 0.
        assert_eq!(b.replay(0).unwrap().len(), 5);
        // since = 1: events 2..=5 are after 1.
        assert_eq!(b.replay(1).unwrap().len(), 4);
        // since = 4: only event 5 is after 4.
        assert_eq!(b.replay(4).unwrap().len(), 1);
        // since = 5: no events after 5.
        assert_eq!(b.replay(5).unwrap().len(), 0);
    }
}
