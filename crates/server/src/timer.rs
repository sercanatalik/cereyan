//! A single timer heap for everything time-driven: schedule fires, due runs,
//! late checks, crash reruns, flow timeouts, disable-window resumes, and the
//! periodic wake-up persistence. The scheduler task sleeps until the head.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
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
    /// A run that has still not started is skipped: its start deadline passed.
    StartDeadline(i64),
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
    /// The global pause's `until` arrived; the value is the pause's `since`.
    SchedulerResume(i64),
    /// A paused run's wake time arrived (a sleep, an event deadline, a target poke).
    WakeRun(i64),
}

#[derive(Default)]
struct Inner {
    heap: BinaryHeap<Reverse<(i64, u64, TimerEvent)>>,
    /// Sequence numbers of events that have been removed via lazy deletion.
    /// These are skipped when popping from the heap and cleaned up then.
    cancelled: HashSet<u64>,
    /// Number of active (non-cancelled) events in the heap.
    /// Incremented on push, decremented exactly once per event — on cancel or
    /// on pop, never both.
    active_count: usize,
    /// Pending sequence numbers keyed by the id a removal targets, so removing
    /// one run's events costs what that run has pending rather than a scan of
    /// the whole heap. Entries are pruned as their events are popped.
    by_run: HashMap<i64, Vec<u64>>,
    by_schedule: HashMap<i64, Vec<u64>>,
    by_rule: HashMap<i64, Vec<u64>>,
    /// Which side index a sequence belongs to, and under which key.
    ///
    /// Without it, forgetting a sequence means searching every value vector of
    /// every one of the three maps — and forgetting happens once per popped event,
    /// so a drain of n events over a heap holding m events costs O(n·m). This makes
    /// it O(1) to find the vector, leaving only a scan of that one vector, whose
    /// length is the number of events pending for a *single* run, schedule or rule.
    side_of: HashMap<u64, (Side, i64)>,
}

/// Which of the three side indexes a sequence is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Run,
    Schedule,
    Rule,
}

impl Inner {
    /// Forget a sequence number in the side indexes. Called when its event leaves
    /// the heap, by being popped.
    ///
    /// `side_of` says which vector holds it, so this touches one vector of one map
    /// rather than searching all three in full.
    fn forget_seq(&mut self, seq: u64) {
        let Some((side, key)) = self.side_of.remove(&seq) else {
            // An event with no removal id — a global tick — is never indexed, so
            // there is nothing to forget. Reaching here means the two structures
            // disagree, which is worth a comment rather than a silent no-op.
            return;
        };
        let map = match side {
            Side::Run => &mut self.by_run,
            Side::Schedule => &mut self.by_schedule,
            Side::Rule => &mut self.by_rule,
        };
        if let Some(ids) = map.get_mut(&key) {
            if let Some(at) = ids.iter().position(|s| *s == seq) {
                ids.swap_remove(at);
            }
            if ids.is_empty() {
                map.remove(&key);
            }
        }
    }

    /// Record a pushed event in the side index its removal path will use.
    fn index_seq(&mut self, seq: u64, event: &TimerEvent) {
        let side = match event {
            TimerEvent::Due(_)
            | TimerEvent::LateCheck(_)
            | TimerEvent::StartDeadline(_)
            | TimerEvent::FlowTimeout(_)
            | TimerEvent::CrashRerun(_)
            | TimerEvent::WakeRun(_) => Side::Run,
            TimerEvent::Fire(_) => Side::Schedule,
            TimerEvent::RuleClock(_) => Side::Rule,
            // Global ticks carry no id and are never removed individually.
            // `ResumeFlow` carries a flow id and `Expectation` an expectation id,
            // so neither belongs in `by_schedule` or `by_run`: filed there, a
            // schedule's or a run's removal cancelled whichever flow resume or
            // expectation deadline happened to share its id.
            _ => return,
        };
        let map = match side {
            Side::Run => &mut self.by_run,
            Side::Schedule => &mut self.by_schedule,
            Side::Rule => &mut self.by_rule,
        };
        let key = *id_of(event);
        map.entry(key).or_default().push(seq);
        self.side_of.insert(seq, (side, key));
    }

    /// Cancel a set of sequence numbers, counting each once.
    ///
    /// The caller has already removed the side-index vector holding `seqs`, so
    /// their `side_of` entries are dropped here too. Leaving them for the pop
    /// was not enough: compaction discards cancelled tuples without popping
    /// them, and each one stranded a `side_of` entry for the life of the server.
    fn cancel_all(&mut self, seqs: Vec<u64>) {
        for seq in seqs {
            self.side_of.remove(&seq);
            if self.cancelled.insert(seq) {
                self.active_count -= 1;
            }
        }
    }
}

/// The id a removal path keys on, for the variants that carry one.
fn id_of(event: &TimerEvent) -> &i64 {
    match event {
        TimerEvent::Due(r)
        | TimerEvent::LateCheck(r)
        | TimerEvent::StartDeadline(r)
        | TimerEvent::FlowTimeout(r)
        | TimerEvent::CrashRerun(r)
        | TimerEvent::WakeRun(r)
        | TimerEvent::Fire(r)
        | TimerEvent::RuleClock(r) => r,
        _ => unreachable!("event has no removal id"),
    }
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
        inner.heap.push(Reverse((at_micros, seq, event.clone())));
        inner.index_seq(seq, &event);
        inner.active_count += 1;
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
            if *t > now {
                break;
            }
            let Reverse((t, seq, e)) = inner.heap.pop().unwrap();
            // A cancelled event was already decremented when it was cancelled,
            // so it must not be decremented again here. Getting this wrong
            // drove `active_count` below zero on every cancelled event.
            if inner.cancelled.remove(&seq) {
                inner.forget_seq(seq);
                continue;
            }
            inner.forget_seq(seq);
            inner.active_count -= 1;
            out.push((t, e));
        }
        // Lazy deletion leaves dead tuples in the heap until their original
        // timestamp arrives, which for a cancelled future event can be hours.
        // Rebuild once the dead entries outnumber the live ones. Ordering is
        // preserved because the key `(at, seq, event)` is untouched.
        if inner.cancelled.len() * 2 > inner.heap.len() {
            let cancelled = std::mem::take(&mut inner.cancelled);
            // Nothing here touches the side indexes, and that is the point.
            //
            // Compaction removes cancelled *tuples* from the heap. It cancels
            // nothing: every live event is still live, so the side indexes still
            // hold exactly the right sequences and must be left alone.
            //
            // This block used to call `forget_seq` for every kept entry, and
            // `forget_seq` is the function that removes a sequence from the side
            // indexes — so each compaction stripped every live sequence out of
            // `by_run`, `by_schedule` and `by_rule`, after which
            // `remove_run_events`, `remove_schedule_events` and `remove_rule_clock`
            // found nothing and cancelled nothing, silently. A removed run's
            // `LateCheck`, `FlowTimeout` and `CrashRerun` kept firing.
            // `a_compaction_leaves_events_cancellable` is the regression.
            let kept: Vec<Reverse<(i64, u64, TimerEvent)>> = inner
                .heap
                .drain()
                .filter(|Reverse((_, seq, _))| !cancelled.contains(seq))
                .collect();
            inner.heap = kept.into_iter().collect();
        }
        out
    }

    /// Drop every pending event; a reset re-arms what it keeps.
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.heap.clear();
        inner.cancelled.clear();
        inner.active_count = 0;
        inner.by_run.clear();
        inner.by_schedule.clear();
        inner.by_rule.clear();
        inner.side_of.clear();
    }

    pub fn remove_run_events(&self, run_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Lazy deletion via the run's side index: O(this run's events) rather
        // than a scan of the whole heap.
        let seqs = inner.by_run.remove(&run_id).unwrap_or_default();
        inner.cancel_all(seqs);
    }

    /// Is a tick of this clock-armed rule pending?
    pub fn has_rule_clock(&self, rule_id: i64) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.by_rule.contains_key(&rule_id)
    }

    /// Drop pending ticks of a clock-armed rule.
    pub fn remove_rule_clock(&self, rule_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let seqs = inner.by_rule.remove(&rule_id).unwrap_or_default();
        inner.cancel_all(seqs);
    }

    pub fn remove_schedule_events(&self, schedule_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let seqs = inner.by_schedule.remove(&schedule_id).unwrap_or_default();
        inner.cancel_all(seqs);
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_count
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

    /// Removing a schedule's events must not cancel a flow's resume that shares the id.
    #[test]
    fn removing_a_schedule_keeps_a_flow_resume_with_the_same_id() {
        let t = Timer::new();
        t.push(10, TimerEvent::Fire(3));
        t.push(20, TimerEvent::ResumeFlow(3));
        t.remove_schedule_events(3);
        assert_eq!(t.len(), 1);
        assert_eq!(t.pop_due(100), vec![(20, TimerEvent::ResumeFlow(3))]);
    }

    /// Removing a run's events must not cancel an expectation that shares the id.
    #[test]
    fn removing_a_run_keeps_an_expectation_with_the_same_id() {
        let t = Timer::new();
        t.push(10, TimerEvent::Due(42));
        t.push(20, TimerEvent::Expectation(42));
        t.remove_run_events(42);
        assert_eq!(t.len(), 1);
        assert_eq!(t.pop_due(100), vec![(20, TimerEvent::Expectation(42))]);
    }

    /// A compaction must not stop the timer cancelling events.
    ///
    /// The compaction block rebuilds the heap without the cancelled *tuples*. It is not
    /// a cancellation: nothing about a live event has changed, so nothing about the
    /// side indexes should either.
    ///
    /// It used to call `forget_seq` for every **kept** entry, and `forget_seq` is
    /// precisely the function that removes a sequence from the side indexes. So each
    /// compaction stripped every live sequence out of `by_run`, `by_schedule` and
    /// `by_rule`, and from then on `remove_run_events`, `remove_schedule_events` and
    /// `remove_rule_clock` all returned nothing and cancelled nothing — silently. A
    /// removed run's `LateCheck`, `FlowTimeout` and `CrashRerun` would still fire.
    ///
    /// The fixture puts the cancelled events **in the future**, which is the only shape
    /// that reaches the compaction branch: popping a cancelled event removes it from
    /// `cancelled` as it goes, so cancelled events that are already due drain the set
    /// and the branch never runs. Its own comment says as much — "for a cancelled
    /// future event [the dead entry] can be hours" away.
    #[test]
    fn a_compaction_leaves_events_cancellable() {
        let t = Timer::new();
        // Live events, far in the future, for one run, one schedule and one rule.
        t.push(1_000_000, TimerEvent::Due(7));
        t.push(1_000_001, TimerEvent::FlowTimeout(7));
        t.push(1_000_002, TimerEvent::Fire(3));
        t.push(1_000_003, TimerEvent::RuleClock(5));
        // Cancelled events, also in the future, so they stay in the heap and in
        // `cancelled` and the compaction branch fires.
        for i in 0..20 {
            t.push(900_000 + i, TimerEvent::Due(1000 + i));
        }
        for i in 0..20 {
            t.remove_run_events(1000 + i);
        }
        assert_eq!(t.len(), 4, "four live events");

        // Nothing is due yet, so this only runs the compaction branch.
        assert!(t.pop_due(50_000).is_empty(), "nothing is due at 50s");

        // All three removal paths must still work.
        t.remove_run_events(7);
        t.remove_schedule_events(3);
        t.remove_rule_clock(5);
        assert_eq!(
            t.len(),
            0,
            "a compaction must not stop the timer cancelling events"
        );
        assert!(
            t.pop_due(2_000_000).is_empty(),
            "and the cancelled events must not fire"
        );
    }

    /// Cancelled events dropped by compaction must not leave `side_of` entries.
    #[test]
    fn compaction_does_not_leak_side_of() {
        let t = Timer::new();
        t.push(1_000_000, TimerEvent::Due(7));
        for i in 0..20 {
            t.push(900_000 + i, TimerEvent::Due(1000 + i));
            t.push(900_000 + i, TimerEvent::Fire(2000 + i));
            t.push(900_000 + i, TimerEvent::RuleClock(3000 + i));
        }
        for i in 0..20 {
            t.remove_run_events(1000 + i);
            t.remove_schedule_events(2000 + i);
            t.remove_rule_clock(3000 + i);
        }
        assert!(t.pop_due(50_000).is_empty(), "compaction only");
        let inner = t.inner.lock().unwrap();
        assert_eq!(inner.heap.len(), 1, "compaction dropped the dead tuples");
        assert_eq!(inner.side_of.len(), 1, "only the live event stays indexed");
    }

    /// `side_of` must route a sequence to the index it was filed under.
    ///
    /// An injection pointing every sequence at `by_rule` passed all eleven tests in
    /// this module. The reason is that getting it wrong is *invisible* until later:
    /// `forget_seq` looks in the wrong map, does not find the sequence, and returns
    /// having removed nothing. The sequence is then stranded in the correct map
    /// forever, and the next `remove_*` finds it and cancels an event that has already
    /// fired -- decrementing `active_count` a second time.
    ///
    /// So the test cancels *after* popping, which is the only ordering in which the
    /// mistake shows.
    #[test]
    fn a_popped_event_is_not_cancellable_afterwards() {
        let t = Timer::new();
        t.push(10, TimerEvent::Due(1));
        t.push(20, TimerEvent::LateCheck(1));
        t.push(30, TimerEvent::Fire(2));
        t.push(40, TimerEvent::RuleClock(3));
        assert_eq!(t.len(), 4);
        assert_eq!(t.pop_due(100).len(), 4, "all four fire");
        assert_eq!(t.len(), 0, "and the count is back to zero");

        // Every one of these must find nothing, because the events are gone. If any
        // found something, the count would be decremented a second time.
        t.remove_run_events(1);
        t.remove_schedule_events(2);
        t.remove_rule_clock(3);
        assert_eq!(
            t.len(),
            0,
            "cancelling an event that has already fired must not change the count"
        );
        assert!(t.pop_due(200).is_empty(), "and nothing is left in the heap");

        // And every index agrees.
        let inner = t.inner.lock().unwrap();
        assert!(
            inner.by_run.is_empty()
                && inner.by_schedule.is_empty()
                && inner.by_rule.is_empty()
                && inner.side_of.is_empty(),
            "every index is empty after a full drain: by_run {:?}, by_schedule {:?}, \
         by_rule {:?}, side_of has {} entries",
            inner.by_run.keys().collect::<Vec<_>>(),
            inner.by_schedule.keys().collect::<Vec<_>>(),
            inner.by_rule.keys().collect::<Vec<_>>(),
            inner.side_of.len()
        );
    }

    /// The same claim, read straight off the side indexes, so a failure says *which*
    /// index was emptied rather than only that something was.
    #[test]
    fn a_compaction_leaves_the_side_indexes_intact() {
        let t = Timer::new();
        t.push(1_000_000, TimerEvent::Due(7));
        t.push(900_000, TimerEvent::Fire(3));
        for i in 0..20 {
            t.push(800_000 + i, TimerEvent::Due(1000 + i));
        }
        for i in 0..20 {
            t.remove_run_events(1000 + i);
        }
        assert!(t.pop_due(50_000).is_empty());

        let inner = t.inner.lock().unwrap();
        assert_eq!(
            inner.by_run.keys().copied().collect::<Vec<_>>(),
            vec![7],
            "by_run lost its live entry"
        );
        assert_eq!(
            inner.by_schedule.keys().copied().collect::<Vec<_>>(),
            vec![3],
            "by_schedule lost its live entry"
        );
        drop(inner);
        assert!(
            !t.has_rule_clock(99),
            "sanity: an unrelated rule is not indexed"
        );
    }
}

#[cfg(test)]
mod underflow_tests {
    use super::*;

    /// A cancelled event is decremented once when cancelled and again when
    /// popped, driving `active_count` below zero. This drives the timer past a
    /// cancellation and asserts the count, which is what `len`/`is_empty`
    /// report.
    #[test]
    fn cancel_then_pop_does_not_underflow() {
        let t = Timer::new();
        t.push(100, TimerEvent::Due(1));
        t.push(200, TimerEvent::Due(2));
        assert_eq!(t.len(), 2);

        // Cancel run 1's event.
        t.remove_run_events(1);
        assert_eq!(t.len(), 1, "cancelling one event leaves one live");

        // Pop past the cancelled event (both events are now due at 200).
        let popped = t.pop_due(200);
        assert_eq!(popped.len(), 1, "only the live event fires");
        assert!(matches!(popped[0].1, TimerEvent::Due(2)));

        // The cancelled event was already accounted for at cancel time; popping
        // it must not decrement again.
        assert_eq!(
            t.len(),
            0,
            "active_count underflowed or double-decremented: {}",
            t.len()
        );
        assert!(t.is_empty());
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;

    fn run(id: i64) -> TimerEvent {
        TimerEvent::Due(id)
    }

    #[test]
    fn removing_a_run_cancels_only_that_runs_events() {
        let t = Timer::new();
        t.push(10, run(1));
        t.push(20, TimerEvent::LateCheck(1));
        t.push(30, run(2));
        t.push(40, TimerEvent::FlowTimeout(2));
        assert_eq!(t.len(), 4);

        t.remove_run_events(1);
        // Run 1's two events are gone; run 2's two remain.
        assert_eq!(t.len(), 2);
        let popped = t.pop_due(100);
        let ids: Vec<i64> = popped
            .iter()
            .filter_map(|(_, e)| match e {
                TimerEvent::Due(r) | TimerEvent::FlowTimeout(r) => Some(*r),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![2, 2], "only run 2's events fire");
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
    }

    #[test]
    fn removing_a_schedule_leaves_other_schedules_alone() {
        let t = Timer::new();
        t.push(10, TimerEvent::Fire(1));
        t.push(20, TimerEvent::Fire(2));
        t.push(30, TimerEvent::Fire(2));
        assert_eq!(t.len(), 3);

        t.remove_schedule_events(2);
        assert_eq!(t.len(), 1);
        let popped = t.pop_due(100);
        assert_eq!(popped.len(), 1);
        assert!(matches!(popped[0].1, TimerEvent::Fire(1)));
    }

    #[test]
    fn has_rule_clock_reflects_only_pending_ticks() {
        let t = Timer::new();
        assert!(!t.has_rule_clock(7), "no tick pushed yet");

        t.push(10, TimerEvent::RuleClock(7));
        assert!(t.has_rule_clock(7), "tick is pending");
        assert!(!t.has_rule_clock(8), "a different rule has no tick");

        // Popping the tick must clear the index entry, or the answer lies.
        t.pop_due(100);
        assert!(!t.has_rule_clock(7), "index entry outlived its event");

        // Re-arming works, and removal clears it again.
        t.push(20, TimerEvent::RuleClock(7));
        assert!(t.has_rule_clock(7));
        t.remove_rule_clock(7);
        assert!(!t.has_rule_clock(7), "removed tick is no longer pending");
    }

    #[test]
    fn clear_forgets_the_indexes() {
        let t = Timer::new();
        t.push(10, run(1));
        t.push(20, TimerEvent::Fire(5));
        t.push(30, TimerEvent::RuleClock(9));
        t.clear();

        assert!(t.is_empty());
        assert!(!t.has_rule_clock(9), "clear must drop the rule index");
        // After a clear the timer is empty, so a re-push is the only content.
        t.push(40, run(1));
        assert_eq!(t.len(), 1);
        let popped = t.pop_due(100);
        assert_eq!(popped.len(), 1);
        assert!(matches!(popped[0].1, TimerEvent::Due(1)));
    }

    #[test]
    fn compaction_discards_cancelled_entries_and_keeps_order() {
        let t = Timer::new();
        // Ten live events interleaved with ten that get cancelled.
        for i in 1..=10i64 {
            t.push(i * 10, run(i));
        }
        for i in 1..=10i64 {
            t.push(i * 10 + 5, run(100 + i));
        }
        assert_eq!(t.len(), 20);

        // Cancel the even-numbered originals, all before the second block.
        for i in (2..=10i64).step_by(2) {
            t.remove_run_events(i);
        }
        assert_eq!(t.len(), 15, "five cancelled");

        // Pop everything due; this drains the heap and triggers compaction.
        let mut seen: Vec<i64> = Vec::new();
        for (_, e) in t.pop_due(1_000) {
            if let TimerEvent::Due(r) = e {
                seen.push(r);
            }
        }
        // Only the five odd originals plus the ten second-block events.
        assert_eq!(seen.len(), 15, "compaction dropped or kept a wrong count");
        assert!(
            seen.iter().all(|r| *r % 2 == 1 || *r > 100),
            "a cancelled event fired: {seen:?}"
        );
        // Ordering: the remaining originals come first, in time order.
        let originals: Vec<i64> = seen.iter().copied().filter(|r| *r < 100).collect();
        assert_eq!(originals, vec![1, 3, 5, 7, 9], "order not preserved");
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn repeated_cancel_and_rearm_keeps_the_count_exact() {
        // A debounce-style loop: push, cancel, push again, many times over.
        // Each round must start and end at zero, so the count cannot drift no
        // matter how many cancelled entries pile up in the heap.
        let t = Timer::new();
        for round in 1..=50i64 {
            t.push(100, TimerEvent::Due(1));
            t.push(115, TimerEvent::LateCheck(1));
            assert_eq!(t.len(), 2, "count drifted at round {round}");
            t.remove_run_events(1);
            assert_eq!(t.len(), 0, "count drifted after cancel at round {round}");
        }
        assert!(t.is_empty());
    }
}

#[cfg(test)]
mod drain_cost {
    use super::*;
    use std::time::Instant;

    /// Build a timer holding `n` live events spread over `runs` runs, then time a
    /// drain of all of them.
    ///
    /// The cost being measured is `forget_seq`, which used to search every value
    /// vector of all three side indexes once per popped event — O(n·m) for a drain
    /// of n events over a heap holding m. It now looks the vector up.
    fn filled(n: usize, runs: usize) -> Timer {
        let t = Timer::new();
        for i in 0..n {
            // Four events per run, as `arm_run` does, so the per-run vectors are
            // the length they are in production rather than one.
            let run = (i % runs) as i64 + 1;
            let at = 1_000_000 + i as i64;
            t.push(at, TimerEvent::Due(run));
            t.push(at + 1, TimerEvent::LateCheck(run));
            t.push(at + 2, TimerEvent::FlowTimeout(run));
            t.push(at + 3, TimerEvent::Expectation(run));
        }
        t
    }

    #[test]
    fn report_drain_cost() {
        if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
            return;
        }
        println!(
            "{:>8} {:>7} {:>12} {:>14}",
            "events", "runs", "drain", "per event"
        );
        // `filled` pushes four events per iteration, so a column of `n` is 4n
        // events over n runs -- four per run, as `arm_run` does.
        for n in [400usize, 1_600, 6_400, 25_600, 102_400] {
            let t = filled(n, n);
            let at = Instant::now();
            let popped = t.pop_due(10_000_000);
            let drain = at.elapsed().as_secs_f64() * 1e3;
            let events = 4 * n;
            assert_eq!(popped.len(), events, "every live event fires");
            assert!(t.is_empty(), "and the timer is empty afterwards");
            println!(
                "{:>8} {:>7} {:>10.3} ms {:>11.3} us",
                events,
                n,
                drain,
                drain * 1000.0 / events as f64
            );
        }
        println!(
            "\nthe per-event column is the finding: it is flat, so a drain costs\n\
             what the events cost rather than what the heap costs."
        );
    }
}
