//! A single timer heap for everything time-driven: schedule fires, due runs,
//! late checks, crash reruns, flow timeouts, disable-window resumes, and the
//! periodic wake-up persistence. The scheduler task sleeps until the head.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimerEvent {
    /// Top up materialized runs of a schedule.
    Fire(i64),
    /// A materialized run's scheduled time arrived: dispatch it.
    Due(i64),
    /// Check whether a run started; otherwise mark it Late.
    LateCheck(i64),
    /// Create the follow-up run of a crash chain.
    CrashRerun(i64),
    /// A Running flow exceeded its timeout.
    FlowTimeout(i64),
    /// Resume the schedules of a flow disabled by its failure window.
    ResumeFlow(i64),
    /// Persist the scheduler wake-up time.
    Persist,
    /// A retry delay elapsed for a run (flow-level retry handled by engine; unused here).
    Wake,
    /// An armed expectation of a proactive rule reached its deadline.
    Expectation(i64),
    /// A clock-armed proactive rule's cron tick.
    RuleClock(i64),
}

#[derive(Default)]
struct Inner {
    heap: BinaryHeap<Reverse<(i64, u64, TimerEvent)>>,
}

pub struct Timer {
    inner: Mutex<Inner>,
    seq: AtomicU64,
    pub notify: Notify,
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}

impl Timer {
    pub fn new() -> Timer {
        Timer {
            inner: Mutex::new(Inner::default()),
            seq: AtomicU64::new(0),
            notify: Notify::new(),
        }
    }

    pub fn push(&self, at_micros: i64, event: TimerEvent) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let earlier = inner
            .heap
            .peek()
            .map(|Reverse((t, _, _))| at_micros < *t)
            .unwrap_or(true);
        inner.heap.push(Reverse((at_micros, seq, event)));
        drop(inner);
        if earlier {
            self.notify.notify_one();
        }
    }

    pub fn peek_at(&self) -> Option<i64> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .heap
            .peek()
            .map(|Reverse((t, _, _))| *t)
    }

    /// Pop every event due at or before `now`.
    pub fn pop_due(&self, now: i64) -> Vec<(i64, TimerEvent)> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        while let Some(Reverse((t, _, _))) = inner.heap.peek() {
            if *t <= now {
                let Reverse((t, _, e)) = inner.heap.pop().unwrap();
                out.push((t, e));
            } else {
                break;
            }
        }
        out
    }

    pub fn remove_run_events(&self, run_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let kept: Vec<_> = inner
            .heap
            .drain()
            .filter(|Reverse((_, _, e))| !matches!(e, TimerEvent::Due(r) | TimerEvent::LateCheck(r) | TimerEvent::FlowTimeout(r) if *r == run_id))
            .collect();
        inner.heap = kept.into_iter().collect();
    }

    /// Is a tick of this clock-armed rule pending?
    pub fn has_rule_clock(&self, rule_id: i64) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .heap
            .iter()
            .any(|Reverse((_, _, e))| matches!(e, TimerEvent::RuleClock(id) if *id == rule_id))
    }

    /// Drop pending ticks of a clock-armed rule.
    pub fn remove_rule_clock(&self, rule_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let kept: Vec<_> = inner
            .heap
            .drain()
            .filter(|Reverse((_, _, e))| !matches!(e, TimerEvent::RuleClock(id) if *id == rule_id))
            .collect();
        inner.heap = kept.into_iter().collect();
    }

    pub fn remove_schedule_events(&self, schedule_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let kept: Vec<_> = inner
            .heap
            .drain()
            .filter(|Reverse((_, _, e))| !matches!(e, TimerEvent::Fire(s) if *s == schedule_id))
            .collect();
        inner.heap = kept.into_iter().collect();
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .heap
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pops_in_time_order() {
        let t = Timer::new();
        t.push(30, TimerEvent::Due(3));
        t.push(10, TimerEvent::Due(1));
        t.push(20, TimerEvent::Fire(2));
        assert_eq!(t.peek_at(), Some(10));
        let due = t.pop_due(20);
        assert_eq!(due.len(), 2);
        assert_eq!(due[0].1, TimerEvent::Due(1));
        assert_eq!(due[1].1, TimerEvent::Fire(2));
        t.remove_run_events(3);
        assert!(t.is_empty());
    }
}
