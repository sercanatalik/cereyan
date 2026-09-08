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
    flow_project: HashMap<i64, String>,
}

#[derive(Default)]
pub struct ActiveIndex {
    inner: RwLock<Inner>,
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
        flow_projects: Vec<(i64, String)>,
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

    pub fn set_flow_project(&self, flow_id: i64, project: String) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.flow_project.insert(flow_id, project);
    }

    pub fn remove_flow(&self, flow_id: i64) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.flow_project.remove(&flow_id);
        inner.by_flow.remove(&flow_id);
        inner.active.retain(|_, r| r.flow_id != flow_id);
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
    }

    /// Apply a state change: move counters and update or drop the active entry.
    pub fn transition(&self, run: &Run, previous: Option<&State>) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let flow_counts = inner.by_flow.entry(run.flow_id).or_default();
        if let Some(prev) = previous {
            let n = flow_counts.entry(prev.state_type).or_insert(0);
            *n = (*n - 1).max(0);
        }
        *flow_counts.entry(run.state.state_type).or_insert(0) += 1;
        if run.state.is_terminal() {
            inner.active.remove(&run.id);
        } else if let Some(entry) = inner.active.get_mut(&run.id) {
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
        }
    }

    pub fn remove_run(&self, run_id: i64, state: Option<&State>, flow_id: i64) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.active.remove(&run_id);
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

    pub fn update<F: FnOnce(&mut ActiveRun)>(&self, run_id: i64, f: F) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        match inner.active.get_mut(&run_id) {
            Some(r) => {
                f(r);
                true
            }
            None => false,
        }
    }

    pub fn heartbeat(&self, run_id: i64) -> Option<bool> {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.active.get_mut(&run_id).map(|r| {
            r.last_heartbeat = Instant::now();
            r.cancel_requested
        })
    }

    pub fn counts(&self, project: Option<&str>) -> Counts {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut runs: HashMap<String, i64> = HashMap::new();
        let mut flows: HashMap<String, HashMap<String, i64>> = HashMap::new();
        for (flow_id, per_state) in &inner.by_flow {
            if let Some(p) = project {
                if inner.flow_project.get(flow_id).map(|s| s.as_str()) != Some(p) {
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
                    .map(|p| inner.flow_project.get(&r.flow_id).map(|s| s.as_str()) == Some(p))
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
