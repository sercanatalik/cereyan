//! In-memory working set: non-terminal runs and counters, rebuilt on start.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

use cereyan_core::{Run, State, StateType};
use serde::Serialize;

use crate::supervisor::EngineKey;

#[derive(Clone, Debug)]
pub struct ActiveRun {
    pub id: i64,
    pub flow_id: i64,
    pub name: String,
    pub state: State,
    pub engine_pid: Option<u32>,
    pub engine_id: Option<String>,
    pub last_heartbeat: Instant,
    pub cancel_requested: bool,
    pub cancelling_since: Option<Instant>,
    pub terminated_at: Option<Instant>,
    pub key: EngineKey,
    pub adopted: bool,
}

#[derive(Default)]
struct Inner {
    active: HashMap<i64, ActiveRun>,
    by_flow: HashMap<i64, HashMap<StateType, i64>>,
    task_by_state: HashMap<StateType, i64>,
    flow_project: HashMap<i64, std::sync::Arc<str>>,
    /// Ids of runs Paused in the `AwaitingEvent` sub-state, so event delivery
    /// costs O(waiters) rather than O(active runs).
    awaiting_event: std::collections::HashSet<i64>,
}

/// Is this state a run waiting on a durable event?
fn is_awaiting_event(state: &State) -> bool {
    state.state_type == StateType::Paused && state.name == "AwaitingEvent"
}

pub struct ActiveIndex {
    inner: RwLock<Inner>,
    /// Mirrors `!inner.awaiting_event.is_empty()`. Maintained under the same
    /// lock as the set, so it cannot drift from it, and lets the event path
    /// skip the lock entirely when nothing is waiting.
    has_event_waiters: std::sync::atomic::AtomicBool,
    /// Seconds from a run's scheduled time to its start.
    pub start_delay: crate::metrics::Histogram,
    /// Seconds a run spent in AwaitingResource before moving on.
    pub resource_wait: crate::metrics::Histogram,
}

impl Default for ActiveIndex {
    fn default() -> ActiveIndex {
        ActiveIndex {
            inner: RwLock::new(Inner::default()),
            has_event_waiters: std::sync::atomic::AtomicBool::new(false),
            start_delay: crate::metrics::Histogram::new(crate::metrics::DELAY_BOUNDS),
            resource_wait: crate::metrics::Histogram::new(crate::metrics::DELAY_BOUNDS),
        }
    }
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct Counts {
    /// Run counts by state type.
    pub runs: HashMap<String, i64>,
    /// Run counts by flow id and state type.
    pub flows: HashMap<String, HashMap<String, i64>>,
    /// Task run counts by state type.
    pub task_runs: HashMap<String, i64>,
    /// Number of non-terminal runs held in memory.
    pub active: i64,
}

impl ActiveIndex {
    pub fn new() -> ActiveIndex {
        ActiveIndex::default()
    }

    pub fn load_counts(
        &self,
        runs: Vec<(i64, String, i64)>,
        task_runs: Vec<(String, i64)>,
        flow_projects: Vec<(i64, std::sync::Arc<str>)>,
    ) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.by_flow.clear();
        for (flow_id, state, n) in runs {
            if let Some(t) = StateType::parse(&state) {
                *inner
                    .by_flow
                    .entry(flow_id)
                    .or_default()
                    .entry(t)
                    .or_insert(0) += n;
            }
        }
        inner.task_by_state.clear();
        for (state, n) in task_runs {
            if let Some(t) = StateType::parse(&state) {
                *inner.task_by_state.entry(t).or_insert(0) += n;
            }
        }
        inner.flow_project = flow_projects.into_iter().collect();
    }

    pub fn set_flow_project(&self, flow_id: i64, project: std::sync::Arc<str>) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.flow_project.insert(flow_id, project);
    }

    /// Forget every run held in memory; a reset reloads what is left.
    pub fn clear_active(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.active.clear();
        inner.awaiting_event.clear();
        self.has_event_waiters
            .store(false, std::sync::atomic::Ordering::Release);
    }

    pub fn remove_flow(&self, flow_id: i64) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.flow_project.remove(&flow_id);
        inner.by_flow.remove(&flow_id);
        // Collect first: `retain` holds a borrow of `active`, so the waiter set
        // can only be touched afterwards.
        let dropped: Vec<i64> = inner
            .active
            .iter()
            .filter(|(_, r)| r.flow_id == flow_id)
            .map(|(id, _)| *id)
            .collect();
        inner.active.retain(|_, r| r.flow_id != flow_id);
        for id in dropped {
            self.set_waiting(&mut inner, id, false);
        }
    }

    /// Record a run that was just created (its first state is already set).
    pub fn insert_run(&self, run: &Run, key: EngineKey, adopted: bool) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *inner
            .by_flow
            .entry(run.flow_id)
            .or_default()
            .entry(run.state.state_type)
            .or_insert(0) += 1;
        if !run.state.is_terminal() {
            inner.active.insert(
                run.id,
                ActiveRun {
                    id: run.id,
                    flow_id: run.flow_id,
                    name: run.name.clone(),
                    state: run.state.clone(),
                    engine_pid: run.engine_pid.map(|p| p as u32),
                    engine_id: run.engine_id.clone(),
                    last_heartbeat: Instant::now(),
                    cancel_requested: run.state.state_type == StateType::Cancelling,
                    cancelling_since: if run.state.state_type == StateType::Cancelling {
                        Some(Instant::now())
                    } else {
                        None
                    },
                    terminated_at: None,
                    key,
                    adopted,
                },
            );
            if is_awaiting_event(&run.state) {
                self.set_waiting(&mut inner, run.id, true);
            }
        }
    }

    /// Insert a run whose count is already part of the loaded counters
    /// (used during reconciliation on start).
    pub fn adopt_run(&self, run: &Run, key: EngineKey) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.active.insert(
            run.id,
            ActiveRun {
                id: run.id,
                flow_id: run.flow_id,
                name: run.name.clone(),
                state: run.state.clone(),
                engine_pid: run.engine_pid.map(|p| p as u32),
                engine_id: run.engine_id.clone(),
                last_heartbeat: Instant::now(),
                cancel_requested: run.state.state_type == StateType::Cancelling,
                cancelling_since: if run.state.state_type == StateType::Cancelling {
                    Some(Instant::now())
                } else {
                    None
                },
                terminated_at: None,
                key,
                adopted: true,
            },
        );
        if is_awaiting_event(&run.state) {
            self.set_waiting(&mut inner, run.id, true);
        }
    }

    /// Apply a state change: move counters and update or drop the active entry.
    pub fn transition(&self, run: &Run, previous: Option<&State>) {
        let now = cereyan_core::now_micros();
        if run.state.state_type == StateType::Running
            && previous.is_some_and(|p| p.state_type != StateType::Running)
        {
            if let Some(scheduled) = run.scheduled_time {
                self.start_delay
                    .observe((now - scheduled) as f64 / 1_000_000.0);
            }
        }
        if let Some(prev) = previous {
            if prev.name == "AwaitingResource" && run.state.name != "AwaitingResource" {
                self.resource_wait
                    .observe((now - prev.timestamp) as f64 / 1_000_000.0);
            }
        }
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let flow_counts = inner.by_flow.entry(run.flow_id).or_default();
        if let Some(prev) = previous {
            let n = flow_counts.entry(prev.state_type).or_insert(0);
            *n = (*n - 1).max(0);
        }
        *flow_counts.entry(run.state.state_type).or_insert(0) += 1;
        if run.state.is_terminal() {
            inner.active.remove(&run.id);
            self.set_waiting(&mut inner, run.id, false);
        } else {
            self.set_waiting(&mut inner, run.id, is_awaiting_event(&run.state));
            if let Some(entry) = inner.active.get_mut(&run.id) {
                entry.state = run.state.clone();
                entry.engine_pid = run.engine_pid.map(|p| p as u32).or(entry.engine_pid);
                if run.state.state_type == StateType::Cancelling {
                    entry.cancel_requested = true;
                    if entry.cancelling_since.is_none() {
                        entry.cancelling_since = Some(Instant::now());
                    }
                }
            }
        }
    }

    /// Move many runs of one flow from `previous` to a terminal `next` state.
    pub fn bulk_terminal(
        &self,
        flow_id: i64,
        run_ids: &[i64],
        previous: StateType,
        next: StateType,
    ) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let n = run_ids.len() as i64;
        let counts = inner.by_flow.entry(flow_id).or_default();
        let p = counts.entry(previous).or_insert(0);
        *p = (*p - n).max(0);
        *counts.entry(next).or_insert(0) += n;
        for id in run_ids {
            inner.active.remove(id);
            self.set_waiting(&mut inner, *id, false);
        }
    }

    pub fn remove_run(&self, run_id: i64, state: Option<&State>, flow_id: i64) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.active.remove(&run_id);
        self.set_waiting(&mut inner, run_id, false);
        if let Some(s) = state {
            if let Some(n) = inner
                .by_flow
                .get_mut(&flow_id)
                .and_then(|m| m.get_mut(&s.state_type))
            {
                *n = (*n - 1).max(0);
            }
        }
    }

    pub fn task_run_transition(&self, previous: Option<StateType>, next: StateType) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = previous {
            let n = inner.task_by_state.entry(p).or_insert(0);
            *n = (*n - 1).max(0);
        }
        *inner.task_by_state.entry(next).or_insert(0) += 1;
    }

    pub fn get(&self, run_id: i64) -> Option<ActiveRun> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .get(&run_id)
            .cloned()
    }

    pub fn active_runs(&self) -> Vec<ActiveRun> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .values()
            .cloned()
            .collect()
    }

    /// Active runs of one flow. Clones only this flow's rows rather than the
    /// whole active set, which is what a per-flow read used to do.
    pub fn active_runs_for_flow(&self, flow_id: i64) -> Vec<ActiveRun> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .values()
            .filter(|r| r.flow_id == flow_id)
            .cloned()
            .collect()
    }

    /// Every active run, grouped by flow, in one pass. For callers that need
    /// several flows' runs at once.
    pub fn active_runs_by_flow(&self) -> HashMap<i64, Vec<ActiveRun>> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut by_flow: HashMap<i64, Vec<ActiveRun>> = HashMap::new();
        for r in inner.active.values() {
            by_flow.entry(r.flow_id).or_default().push(r.clone());
        }
        by_flow
    }

    pub fn update<F: FnOnce(&mut ActiveRun)>(&self, run_id: i64, f: F) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        // Mutate first, then re-borrow to read the resulting state: `f` needs
        // `&mut ActiveRun`, so the set cannot be touched inside the same scope.
        let Some(r) = inner.active.get_mut(&run_id) else {
            return false;
        };
        f(r);
        let waiting = is_awaiting_event(&r.state);
        self.set_waiting(&mut inner, run_id, waiting);
        true
    }

    /// Record or clear one run's membership, keeping the fast-path flag in step.
    /// The flag lives outside the lock but is only ever written here, while
    /// `inner` is held, so it can never disagree with the set.
    fn set_waiting(&self, inner: &mut Inner, run_id: i64, waiting: bool) {
        if waiting {
            inner.awaiting_event.insert(run_id);
        } else {
            inner.awaiting_event.remove(&run_id);
        }
        self.has_event_waiters.store(
            !inner.awaiting_event.is_empty(),
            std::sync::atomic::Ordering::Release,
        );
    }

    /// Is any run waiting on a durable event? One relaxed atomic load, no lock.
    pub fn has_event_waiters(&self) -> bool {
        self.has_event_waiters
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Ids of runs waiting on a durable event. O(waiters), not O(active runs).
    pub fn awaiting_event_ids(&self) -> Vec<i64> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .awaiting_event
            .iter()
            .copied()
            .collect()
    }

    /// How many runs are waiting on a durable event.
    pub fn awaiting_event_count(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .awaiting_event
            .len()
    }

    pub fn heartbeat(&self, run_id: i64) -> Option<bool> {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.active.get_mut(&run_id).map(|r| {
            r.last_heartbeat = Instant::now();
            r.cancel_requested
        })
    }

    /// Run totals by state type, with no string allocation.
    ///
    /// `counts` builds the serialised `String`-keyed aggregate; callers that
    /// need only the totals (the metrics sampler, the projects summary) use
    /// this instead so they never pay for the per-flow map.
    pub fn runs_by_state(&self, project: Option<&str>) -> HashMap<StateType, i64> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut runs: HashMap<StateType, i64> = HashMap::new();
        for (flow_id, per_state) in &inner.by_flow {
            if let Some(p) = project {
                if inner.flow_project.get(flow_id).map(|s| s.as_ref()) != Some(p) {
                    continue;
                }
            }
            for (state, n) in per_state {
                *runs.entry(*state).or_insert(0) += n;
            }
        }
        // Every state type is present, as in `counts`.
        for t in StateType::ALL {
            runs.entry(t).or_insert(0);
        }
        runs
    }

    /// Total runs per project, computed in a single pass over the flows.
    ///
    /// Replaces calling `counts` once per project, which rebuilt the whole
    /// nested aggregate each time and then discarded every other project's
    /// flows.
    pub fn run_totals_by_project(&self) -> HashMap<std::sync::Arc<str>, i64> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut totals: HashMap<std::sync::Arc<str>, i64> = HashMap::new();
        for (flow_id, per_state) in &inner.by_flow {
            let Some(project) = inner.flow_project.get(flow_id) else {
                continue;
            };
            let n: i64 = per_state.values().sum();
            *totals.entry(project.clone()).or_insert(0) += n;
        }
        totals
    }

    /// Runs currently held in memory, optionally restricted to one project.
    pub fn active_count(&self, project: Option<&str>) -> i64 {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner
            .active
            .values()
            .filter(|r| match project {
                Some(p) => inner.flow_project.get(&r.flow_id).map(|s| s.as_ref()) == Some(p),
                None => true,
            })
            .count() as i64
    }

    pub fn counts(&self, project: Option<&str>) -> Counts {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut runs: HashMap<String, i64> = HashMap::new();
        let mut flows: HashMap<String, HashMap<String, i64>> = HashMap::new();
        for (flow_id, per_state) in &inner.by_flow {
            if let Some(p) = project {
                if inner.flow_project.get(flow_id).map(|s| s.as_ref()) != Some(p) {
                    continue;
                }
            }
            let mut entry = HashMap::new();
            for (state, n) in per_state {
                *runs.entry(state.as_str().to_string()).or_insert(0) += n;
                entry.insert(state.as_str().to_string(), *n);
            }
            flows.insert(flow_id.to_string(), entry);
        }
        for t in StateType::ALL {
            runs.entry(t.as_str().to_string()).or_insert(0);
        }
        let task_runs = inner
            .task_by_state
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), *v))
            .collect();
        let active = inner
            .active
            .values()
            .filter(|r| {
                project
                    .map(|p| inner.flow_project.get(&r.flow_id).map(|s| s.as_ref()) == Some(p))
                    .unwrap_or(true)
            })
            .count() as i64;
        Counts {
            runs,
            flows,
            task_runs,
            active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cereyan_core::new_id;
    use serde_json::Map;

    fn run_in(id: i64, flow_id: i64, state: State) -> Run {
        Run {
            id,
            external_id: new_id(),
            flow_id,
            flow_name: "f".into(),
            project: "p".into(),
            group: String::new(),
            name: format!("r{id}"),
            parameters: Map::new(),
            tags: vec![],
            attributes: Map::new(),
            state,
            failure_count: 0,
            crash_count: 0,
            created_at: 0,
            start_time: None,
            end_time: None,
            total_run_time: None,
            engine_pid: None,
            engine_id: None,
            created_by: String::new(),
            report_seq: 0,
            schedule_id: None,
            scheduled_time: None,
            priority: 0,
            parent_run_id: None,
            attempt: 0,
            backfill_id: None,
            task_counts: Default::default(),
            unique_key: None,
            host: None,
            processor: None,
            lease: 0,
            source_hash: None,
        }
    }

    fn awaiting() -> State {
        State::from_parts(StateType::Paused, Some("AwaitingEvent"), None, Map::new())
    }

    fn test_key() -> EngineKey {
        EngineKey {
            source_dir: "/tmp".into(),
            module: "m".into(),
            isolated: false,
            nice: 0,
        }
    }

    /// The waiter set must always agree with a scan of the active set.
    fn assert_agrees(ix: &ActiveIndex) {
        let scanned: std::collections::HashSet<i64> = ix
            .active_runs()
            .iter()
            .filter(|r| is_awaiting_event(&r.state))
            .map(|r| r.id)
            .collect();
        let indexed: std::collections::HashSet<i64> = ix.awaiting_event_ids().into_iter().collect();
        assert_eq!(scanned, indexed, "waiter index drifted from the active set");
        assert_eq!(
            ix.has_event_waiters(),
            !scanned.is_empty(),
            "fast-path flag disagrees with the waiter set"
        );
    }

    #[test]
    fn waiter_index_tracks_insert_and_transition() {
        let ix = ActiveIndex::new();
        let key = test_key();
        assert!(!ix.has_event_waiters());
        assert_eq!(ix.awaiting_event_count(), 0);

        // Insert as a waiting run.
        ix.insert_run(&run_in(1, 10, awaiting()), key.clone(), false);
        assert_eq!(ix.awaiting_event_ids(), vec![1]);
        assert!(ix.has_event_waiters());
        assert_agrees(&ix);

        // A run that is not waiting is never indexed.
        ix.insert_run(
            &run_in(2, 10, State::new(StateType::Running)),
            key.clone(),
            false,
        );
        assert_eq!(ix.awaiting_event_ids(), vec![1]);
        assert_agrees(&ix);

        // Transition out of the waiting sub-state clears membership.
        let mut next = run_in(1, 10, State::new(StateType::Running));
        next.state = State::new(StateType::Running);
        ix.transition(&next, Some(&awaiting()));
        assert_eq!(ix.awaiting_event_count(), 0);
        assert!(!ix.has_event_waiters());
        assert_agrees(&ix);

        // Transition back in.
        let mut back = run_in(1, 10, awaiting());
        back.state = awaiting();
        ix.transition(&back, Some(&State::new(StateType::Running)));
        assert_eq!(ix.awaiting_event_ids(), vec![1]);
        assert_agrees(&ix);
    }

    #[test]
    fn waiter_index_tracks_update_and_removal() {
        let ix = ActiveIndex::new();
        let key = test_key();
        ix.insert_run(&run_in(7, 10, State::new(StateType::Running)), key, false);
        assert!(!ix.has_event_waiters());

        // `update` is the funnel for state changes made through the index.
        ix.update(7, |r| r.state = awaiting());
        assert_eq!(ix.awaiting_event_ids(), vec![7]);
        assert_agrees(&ix);

        ix.update(7, |r| r.state = State::new(StateType::Running));
        assert_eq!(ix.awaiting_event_count(), 0);
        assert_agrees(&ix);

        // Removal drops membership and clears the flag.
        ix.update(7, |r| r.state = awaiting());
        ix.remove_run(7, Some(&awaiting()), 10);
        assert_eq!(ix.awaiting_event_count(), 0);
        assert!(!ix.has_event_waiters());
        assert_agrees(&ix);
    }

    #[test]
    fn waiter_index_cleared_by_flow_removal_and_reset() {
        let ix = ActiveIndex::new();
        let key = test_key();
        ix.insert_run(&run_in(1, 10, awaiting()), key.clone(), false);
        ix.insert_run(&run_in(2, 20, awaiting()), key, false);
        assert_eq!(ix.awaiting_event_count(), 2);

        // Deleting one flow's runs leaves the other flow's waiter.
        ix.remove_flow(10);
        assert_eq!(ix.awaiting_event_ids(), vec![2]);
        assert!(ix.has_event_waiters());
        assert_agrees(&ix);

        // A reset forgets everything, including the flag.
        ix.clear_active();
        assert_eq!(ix.awaiting_event_count(), 0);
        assert!(!ix.has_event_waiters());
        assert_agrees(&ix);
    }

    #[test]
    fn waiter_index_drops_terminal_runs() {
        let ix = ActiveIndex::new();
        let key = test_key();
        ix.insert_run(&run_in(1, 10, awaiting()), key.clone(), false);
        ix.insert_run(&run_in(2, 10, awaiting()), key, false);
        assert_eq!(ix.awaiting_event_count(), 2);

        let mut done = run_in(1, 10, State::new(StateType::Completed));
        done.state = State::new(StateType::Completed);
        ix.transition(&done, Some(&awaiting()));
        assert_eq!(ix.awaiting_event_ids(), vec![2]);
        assert_agrees(&ix);

        // bulk_terminal clears the rest.
        ix.bulk_terminal(10, &[2], StateType::Paused, StateType::Failed);
        assert_eq!(ix.awaiting_event_count(), 0);
        assert!(!ix.has_event_waiters());
        assert_agrees(&ix);
    }
}

#[cfg(test)]
mod counts_tests {
    use super::*;
    use cereyan_core::new_id;
    use serde_json::Map;

    fn run_in(id: i64, flow_id: i64, project: &str, state: State) -> Run {
        Run {
            id,
            external_id: new_id(),
            flow_id,
            flow_name: "f".into(),
            project: project.into(),
            group: String::new(),
            name: format!("r{id}"),
            parameters: Map::new(),
            tags: vec![],
            attributes: Map::new(),
            state,
            failure_count: 0,
            crash_count: 0,
            created_at: 0,
            start_time: None,
            end_time: None,
            total_run_time: None,
            engine_pid: None,
            engine_id: None,
            created_by: String::new(),
            report_seq: 0,
            schedule_id: None,
            scheduled_time: None,
            priority: 0,
            parent_run_id: None,
            attempt: 0,
            backfill_id: None,
            task_counts: Default::default(),
            unique_key: None,
            host: None,
            processor: None,
            lease: 0,
            source_hash: None,
        }
    }

    fn key() -> EngineKey {
        EngineKey {
            source_dir: "/tmp".into(),
            module: "m".into(),
            isolated: false,
            nice: 0,
        }
    }

    /// Two projects, three flows, runs spread across several states.
    fn seeded() -> ActiveIndex {
        let ix = ActiveIndex::new();
        // project a: flows 1, 2      project b: flow 3
        ix.set_flow_project(1, "a".into());
        ix.set_flow_project(2, "a".into());
        ix.set_flow_project(3, "b".into());

        ix.insert_run(
            &run_in(1, 1, "a", State::new(StateType::Completed)),
            key(),
            false,
        );
        ix.insert_run(
            &run_in(2, 1, "a", State::new(StateType::Running)),
            key(),
            false,
        );
        ix.insert_run(
            &run_in(3, 1, "a", State::new(StateType::Failed)),
            key(),
            false,
        );
        ix.insert_run(
            &run_in(4, 2, "a", State::new(StateType::Running)),
            key(),
            false,
        );
        ix.insert_run(
            &run_in(5, 2, "a", State::new(StateType::Completed)),
            key(),
            false,
        );
        ix.insert_run(
            &run_in(6, 3, "b", State::new(StateType::Completed)),
            key(),
            false,
        );
        ix.insert_run(
            &run_in(7, 3, "b", State::new(StateType::Running)),
            key(),
            false,
        );
        ix
    }

    #[test]
    fn runs_by_state_agrees_with_counts_for_every_filter() {
        let ix = seeded();
        for project in [None, Some("a"), Some("b"), Some("missing")] {
            let full = ix.counts(project);
            let proj = ix.runs_by_state(project);
            // Every state in the serialised aggregate must match the projection.
            for t in StateType::ALL {
                let from_counts = full.runs.get(t.as_str()).copied().unwrap_or(0);
                assert_eq!(
                    proj.get(&t).copied().unwrap_or(0),
                    from_counts,
                    "state {t:?} disagrees for project {project:?}"
                );
            }
            // And no extra states in the projection.
            assert_eq!(
                proj.len(),
                StateType::ALL.len(),
                "projection has extra states"
            );
        }
    }

    #[test]
    fn run_totals_by_project_sums_each_project() {
        let ix = seeded();
        let totals = ix.run_totals_by_project();
        assert_eq!(totals.get("a").copied(), Some(5));
        assert_eq!(totals.get("b").copied(), Some(2));

        // Each project total must equal the filtered `counts` sum.
        for (name, total) in &totals {
            let filtered = ix.counts(Some(name));
            assert_eq!(
                *total,
                filtered.runs.values().sum::<i64>(),
                "total for {name} disagrees with counts"
            );
        }
    }

    #[test]
    fn active_count_matches_counts_and_respects_the_filter() {
        let ix = seeded();
        for project in [None, Some("a"), Some("b")] {
            assert_eq!(
                ix.active_count(project),
                ix.counts(project).active,
                "active count disagrees for {project:?}"
            );
        }
        // Only non-terminal runs are held in memory: the three Running ones.
        assert_eq!(ix.active_count(None), 3);
    }
}
