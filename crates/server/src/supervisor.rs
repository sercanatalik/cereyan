//! Engine supervisor: a warm pool of Python engine processes keyed by source
//! directory, module, and isolation; a pull-based work queue; heartbeat and
//! exit monitoring; cancellation escalation; restart adoption.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use cereyan_core::{Flow, Run, State, StateType};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{watch, Notify};

use crate::process;
use crate::state::AppState;
use crate::ServeConfig;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EngineKey {
    pub source_dir: String,
    pub module: String,
    #[serde(default)]
    pub isolated: bool,
    /// OS niceness of the engine (Unix); runs with negative priority go to
    /// engines started with `min(19, -priority)`.
    #[serde(default)]
    pub nice: u8,
}

/// Niceness for a priority: negative priorities lower the OS priority.
pub fn nice_for(priority: i64) -> u8 {
    if priority < 0 {
        (-priority).min(19) as u8
    } else {
        0
    }
}

impl EngineKey {
    pub fn from_flow(flow: &Flow) -> EngineKey {
        let priority = flow
            .options
            .get("priority")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        EngineKey {
            source_dir: flow.source_dir.clone(),
            module: flow.module.clone(),
            isolated: flow
                .options
                .get("isolated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            nice: nice_for(priority),
        }
    }

    pub fn module_path(&self) -> std::path::PathBuf {
        let rel = self.module.replace('.', "/");
        let base = Path::new(&self.source_dir);
        let file = base.join(format!("{rel}.py"));
        if file.exists() {
            file
        } else {
            base.join(rel).join("__init__.py")
        }
    }

    pub fn module_mtime(&self) -> Option<SystemTime> {
        std::fs::metadata(self.module_path())
            .ok()
            .and_then(|m| m.modified().ok())
    }
}

#[derive(Debug)]
pub struct Engine {
    pub id: String,
    pub key: EngineKey,
    pub pid: Option<u32>,
    pub child: Option<Child>,
    pub runs_done: u32,
    pub module_mtime: Option<SystemTime>,
    pub current_run: Option<i64>,
    pub exit_requested: bool,
    pub spawned_at: Instant,
    pub last_seen: Instant,
    pub adopted: bool,
}

#[derive(Clone, Debug)]
pub struct QueuedRun {
    pub run_id: i64,
    pub key: EngineKey,
    pub priority: i64,
    /// Scheduled time or creation time: ties are dispatched oldest first.
    pub order: i64,
    pub needs: Vec<(String, f64)>,
    /// Do not dispatch before this time (microseconds); engines may pre-warm.
    pub not_before: Option<i64>,
}

/// A non-run job for an engine of a key (hooks, bulk_complete prefilter).
#[derive(Clone, Debug)]
pub struct QueuedJob {
    pub key: EngineKey,
    pub payload: serde_json::Value,
}

#[derive(Default)]
pub struct Resources {
    pub totals: HashMap<String, f64>,
    pub used: HashMap<String, f64>,
    /// Leases per run: (lease id, name, amount).
    pub leases: HashMap<i64, Vec<(u64, String, f64)>>,
    next_lease: u64,
}

impl Resources {
    pub fn total(&self, name: &str) -> f64 {
        *self.totals.get(name).unwrap_or(&1.0)
    }
    pub fn free(&self, name: &str) -> f64 {
        self.total(name) - *self.used.get(name).unwrap_or(&0.0)
    }
    pub fn can_acquire(&self, needs: &[(String, f64)]) -> Option<String> {
        needs
            .iter()
            .find(|(n, a)| self.free(n) + 1e-9 < *a)
            .map(|(n, _)| n.clone())
    }
    pub fn acquire(&mut self, run_id: i64, needs: &[(String, f64)]) -> u64 {
        self.next_lease += 1;
        let lease = self.next_lease;
        for (n, a) in needs {
            *self.used.entry(n.clone()).or_insert(0.0) += a;
            self.leases
                .entry(run_id)
                .or_default()
                .push((lease, n.clone(), *a));
        }
        lease
    }
    pub fn release_lease(&mut self, run_id: i64, lease: u64) {
        if let Some(list) = self.leases.get_mut(&run_id) {
            let mut kept = Vec::new();
            for (l, n, a) in list.drain(..) {
                if l == lease {
                    if let Some(u) = self.used.get_mut(&n) {
                        *u = (*u - a).max(0.0);
                    }
                } else {
                    kept.push((l, n, a));
                }
            }
            *list = kept;
        }
    }
    pub fn release_all(&mut self, run_id: i64) {
        if let Some(list) = self.leases.remove(&run_id) {
            for (_, n, a) in list {
                if let Some(u) = self.used.get_mut(&n) {
                    *u = (*u - a).max(0.0);
                }
            }
        }
    }
}

#[derive(Default)]
struct Inner {
    engines: HashMap<String, Engine>,
    queue: VecDeque<QueuedRun>,
    jobs: VecDeque<QueuedJob>,
    next_engine: u64,
    /// Runs already reported as waiting for a resource (avoid repeated transitions).
    waiting_marked: HashMap<i64, String>,
    failures: HashMap<i64, Vec<i64>>,
}

pub struct Supervisor {
    inner: Mutex<Inner>,
    pub resources: Mutex<Resources>,
    pub notify: Notify,
    python: String,
    home: std::path::PathBuf,
    token: Option<String>,
    pub max_engines: usize,
    engine_max_runs: u32,
    pub cancel_grace: Duration,
    pub heartbeat: Duration,
}

/// Work handed to an engine.
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkItem {
    /// `run`, `hooks`, or `bulk_complete`.
    #[serde(default = "default_kind")]
    pub kind: String,
    pub run_id: i64,
    pub run_name: String,
    pub external_id: String,
    pub project: String,
    pub flow: String,
    #[schema(value_type = Object)]
    pub parameters: serde_json::Value,
    #[schema(value_type = Object)]
    pub options: serde_json::Value,
    pub cancel_requested: bool,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
}

fn default_kind() -> String {
    "run".into()
}

impl Supervisor {
    pub fn new(config: &ServeConfig) -> Supervisor {
        let mut resources = Resources::default();
        for (name, total) in &config.resources {
            resources.totals.insert(name.clone(), *total);
        }
        Supervisor {
            inner: Mutex::new(Inner::default()),
            resources: Mutex::new(resources),
            notify: Notify::new(),
            python: config.python.clone(),
            home: config.home.clone(),
            token: config.token.clone(),
            max_engines: config.max_engines.max(1),
            engine_max_runs: config.engine_max_runs.max(1),
            cancel_grace: Duration::from_secs(config.cancel_grace_secs.max(1)),
            heartbeat: Duration::from_secs(config.heartbeat_secs.max(1)),
        }
    }

    pub fn enqueue(&self, item: QueuedRun) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.queue.iter().any(|q| q.run_id == item.run_id) {
            inner.queue.push_back(item);
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    /// Queue many runs at once (backfills), notifying waiters once.
    pub fn enqueue_many(&self, items: Vec<QueuedRun>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let existing: std::collections::HashSet<i64> =
            inner.queue.iter().map(|q| q.run_id).collect();
        for item in items {
            if !existing.contains(&item.run_id) {
                inner.queue.push_back(item);
            }
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    /// Convenience for callers that only know the run and key.
    pub fn enqueue_simple(&self, run_id: i64, key: EngineKey) {
        self.enqueue(QueuedRun {
            run_id,
            key,
            priority: 0,
            order: cereyan_core::now_micros(),
            needs: Vec::new(),
            not_before: None,
        });
    }

    pub fn enqueue_job(&self, key: EngineKey, payload: serde_json::Value) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.jobs.push_back(QueuedJob { key, payload });
        drop(inner);
        self.notify.notify_waiters();
    }

    // ---- resources ----------------------------------------------------------

    pub fn set_total(&self, name: &str, total: f64) {
        let mut r = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        r.totals.insert(name.to_string(), total);
    }

    pub fn set_totals(&self, totals: &HashMap<String, f64>) {
        let mut r = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        for (k, v) in totals {
            r.totals.insert(k.clone(), *v);
        }
        drop(r);
        self.notify.notify_waiters();
    }

    pub fn available(&self, name: &str, amount: f64) -> bool {
        let r = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        r.free(name) + 1e-9 >= amount
    }

    /// Try to lease task-level resources for a run. Returns the lease id or the blocking name.
    pub fn try_acquire(
        &self,
        run_id: i64,
        needs: &[(String, f64)],
    ) -> std::result::Result<u64, String> {
        let mut r = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(blocking) = r.can_acquire(needs) {
            return Err(blocking);
        }
        Ok(r.acquire(run_id, needs))
    }

    pub fn release_lease(&self, run_id: i64, lease: u64) {
        let mut r = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        r.release_lease(run_id, lease);
        drop(r);
        self.notify.notify_waiters();
    }

    pub fn resources_snapshot(&self) -> serde_json::Value {
        let r = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<&String> = r.totals.keys().chain(r.used.keys()).collect();
        names.sort();
        names.dedup();
        serde_json::Value::Object(
            names
                .into_iter()
                .map(|n| {
                    (
                        n.clone(),
                        serde_json::json!({"total": r.total(n), "used": r.used.get(n).copied().unwrap_or(0.0)}),
                    )
                })
                .collect(),
        )
    }

    pub fn resource_totals(&self) -> HashMap<String, f64> {
        self.resources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .totals
            .clone()
    }

    /// Record a failure of a flow and return how many fell inside the window.
    pub fn record_failure(&self, flow_id: i64, now: i64, window_micros: i64) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let list = inner.failures.entry(flow_id).or_default();
        list.push(now);
        list.retain(|t| now - *t <= window_micros);
        list.len()
    }

    pub fn clear_failures(&self, flow_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.failures.remove(&flow_id);
    }

    pub fn forget_pid(&self, pid: u32) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.engines.retain(|_, e| e.pid != Some(pid));
    }

    /// Runs queued and waiting for a resource that were not yet marked.
    pub fn take_waiting_marks(&self) -> Vec<(i64, String)> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        let now = cereyan_core::now_micros();
        let resources = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        let queued: Vec<QueuedRun> = inner.queue.iter().cloned().collect();
        let engines_full = inner.engines.len() >= self.max_engines
            && inner
                .engines
                .values()
                .all(|e| e.current_run.is_some() || e.exit_requested);
        for q in queued {
            if q.not_before.map(|t| t > now).unwrap_or(false) {
                continue;
            }
            let reason = match resources.can_acquire(&q.needs) {
                Some(name) => Some(name),
                None if engines_full
                    && !inner.engines.values().any(|e| {
                        e.key == q.key && e.current_run.is_none() && !e.exit_requested
                    }) =>
                {
                    Some("no engine slot".to_string())
                }
                None => None,
            };
            if let Some(reason) = reason {
                if inner.waiting_marked.get(&q.run_id) != Some(&reason) {
                    inner.waiting_marked.insert(q.run_id, reason.clone());
                    out.push((q.run_id, reason));
                }
            }
        }
        out
    }

    /// Remove a queued run (cancel before pickup). Returns whether it was queued.
    pub fn dequeue(&self, run_id: i64) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = inner.queue.len();
        inner.queue.retain(|q| q.run_id != run_id);
        inner.waiting_marked.remove(&run_id);
        before != inner.queue.len()
    }

    pub fn dequeue_many(&self, run_ids: &[i64]) {
        let set: std::collections::HashSet<i64> = run_ids.iter().copied().collect();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.queue.retain(|q| !set.contains(&q.run_id));
        for id in &set {
            inner.waiting_marked.remove(id);
        }
    }

    pub fn queued(&self, run_id: i64) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .queue
            .iter()
            .any(|q| q.run_id == run_id)
    }

    /// Track an engine that a previous server started.
    pub fn adopt(&self, run: &Run, key: EngineKey) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = run
            .engine_id
            .clone()
            .unwrap_or_else(|| format!("adopted-{}", run.engine_pid.unwrap_or(0)));
        inner.engines.insert(
            id.clone(),
            Engine {
                id,
                key,
                pid: run.engine_pid.map(|p| p as u32),
                child: None,
                runs_done: 0,
                module_mtime: None,
                current_run: Some(run.id),
                exit_requested: false,
                spawned_at: Instant::now(),
                last_seen: Instant::now(),
                adopted: true,
            },
        );
    }

    /// Spawn engines so that every queued run has an engine that can take it,
    /// within `max_engines`. Idle engines of other keys are asked to exit
    /// when the pool is full.
    pub fn ensure_capacity(&self, _state: &AppState) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut demand: HashMap<EngineKey, usize> = HashMap::new();
        let horizon = cereyan_core::now_micros() + crate::scheduler::PREWARM_SECS * 1_000_000;
        {
            let resources = self.resources.lock().unwrap_or_else(|e| e.into_inner());
            for q in &inner.queue {
                if q.not_before.map(|t| t > horizon).unwrap_or(false) {
                    continue;
                }
                if resources.can_acquire(&q.needs).is_some() {
                    continue;
                }
                *demand.entry(q.key.clone()).or_insert(0) += 1;
            }
        }
        for j in &inner.jobs {
            *demand.entry(j.key.clone()).or_insert(0) += 1;
        }
        for (key, pending) in demand {
            let usable = inner
                .engines
                .values()
                .filter(|e| e.key == key && !e.exit_requested && e.current_run.is_none())
                .count();
            let mut to_spawn = pending.saturating_sub(usable);
            while to_spawn > 0 {
                let total = inner.engines.len();
                if total >= self.max_engines {
                    // Evict one idle engine of another key.
                    let victim = inner
                        .engines
                        .values_mut()
                        .find(|e| e.key != key && e.current_run.is_none() && !e.exit_requested);
                    match victim {
                        Some(v) => {
                            v.exit_requested = true;
                        }
                        None => break,
                    }
                    break;
                }
                let id = format!("engine-{}-{}", std::process::id(), inner.next_engine);
                inner.next_engine += 1;
                match self.spawn(&id, &key) {
                    Ok(child) => {
                        inner.engines.insert(
                            id.clone(),
                            Engine {
                                id,
                                pid: Some(child.id()),
                                child: Some(child),
                                runs_done: 0,
                                module_mtime: key.module_mtime(),
                                current_run: None,
                                exit_requested: false,
                                spawned_at: Instant::now(),
                                last_seen: Instant::now(),
                                adopted: false,
                                key: key.clone(),
                            },
                        );
                    }
                    Err(e) => {
                        eprintln!("cereyan: failed to start engine: {e}");
                        break;
                    }
                }
                to_spawn -= 1;
            }
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    fn spawn(&self, id: &str, key: &EngineKey) -> std::io::Result<Child> {
        let mut cmd = Command::new(&self.python);
        cmd.args([
            "-m",
            "cereyan.engine",
            "--engine-id",
            id,
            "--source-dir",
            &key.source_dir,
            "--module",
            &key.module,
        ]);
        if key.isolated {
            cmd.arg("--once");
        }
        if key.nice > 0 {
            cmd.args(["--nice", &key.nice.to_string()]);
        }
        if let Some(token) = &self.token {
            cmd.env("CEREYAN_TOKEN", token);
        }
        cmd.env("CEREYAN_HOME", &self.home)
            .env("CEREYAN_ENGINE_ID", id)
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Own process group: the engine survives the server and is not
            // hit by a terminal Ctrl-C aimed at the server.
            cmd.process_group(0);
        }
        cmd.spawn()
    }

    /// Give an engine its next run, or tell it to exit. Returns Ok(None)
    /// when nothing is queued for its key.
    pub fn take_work(
        &self,
        engine_id: &str,
        pid: u32,
        key: &EngineKey,
        server_url: &str,
    ) -> WorkDecision {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let known = inner.engines.contains_key(engine_id);
        if !known {
            // An engine we did not spawn (previous server) or one that
            // reconnected after a restart: track it.
            inner.engines.insert(
                engine_id.to_string(),
                Engine {
                    id: engine_id.to_string(),
                    key: key.clone(),
                    pid: Some(pid),
                    child: None,
                    runs_done: 0,
                    module_mtime: key.module_mtime(),
                    current_run: None,
                    exit_requested: false,
                    spawned_at: Instant::now(),
                    last_seen: Instant::now(),
                    adopted: true,
                },
            );
        }
        let max_runs = self.engine_max_runs;
        let total = inner.engines.len();
        let over_capacity = total > self.max_engines;
        let engine = inner.engines.get_mut(engine_id).expect("just inserted");
        engine.last_seen = Instant::now();
        engine.pid = Some(pid);
        engine.current_run = None;
        let stale = match (engine.module_mtime, key.module_mtime()) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        if engine.exit_requested
            || engine.runs_done >= max_runs
            || stale
            || (over_capacity && engine.adopted)
        {
            inner.engines.remove(engine_id);
            return WorkDecision::Exit;
        }
        let _ = server_url;
        if let Some(pos) = inner.jobs.iter().position(|j| &j.key == key) {
            let job = inner.jobs.remove(pos).expect("position exists");
            return WorkDecision::Job(job.payload);
        }
        let now = cereyan_core::now_micros();
        let mut candidates: Vec<(usize, i64, i64, i64)> = inner
            .queue
            .iter()
            .enumerate()
            .filter(|(_, q)| &q.key == key && q.not_before.map(|t| t <= now).unwrap_or(true))
            .map(|(i, q)| (i, -q.priority, q.order, q.run_id))
            .collect();
        candidates.sort_by_key(|(_, p, o, id)| (*p, *o, *id));
        let mut resources = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        for (i, _, _, _) in candidates {
            let needs = inner.queue[i].needs.clone();
            if resources.can_acquire(&needs).is_some() {
                continue;
            }
            let item = inner.queue.remove(i).expect("position exists");
            resources.acquire(item.run_id, &needs);
            inner.waiting_marked.remove(&item.run_id);
            if let Some(engine) = inner.engines.get_mut(engine_id) {
                engine.current_run = Some(item.run_id);
            }
            return WorkDecision::Run(item.run_id);
        }
        WorkDecision::Wait
    }

    pub fn run_finished(&self, run_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for e in inner.engines.values_mut() {
            if e.current_run == Some(run_id) {
                e.current_run = None;
                e.runs_done += 1;
                if e.key.isolated {
                    e.exit_requested = true;
                }
            }
        }
        inner.queue.retain(|q| q.run_id != run_id);
        inner.waiting_marked.remove(&run_id);
        drop(inner);
        self.resources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release_all(run_id);
        self.notify.notify_waiters();
    }

    pub fn engine_for_run(&self, run_id: i64) -> Option<(String, Option<u32>)> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .engines
            .values()
            .find(|e| e.current_run == Some(run_id))
            .map(|e| (e.id.clone(), e.pid))
    }

    /// Fail every queued run of a key whose module cannot be imported and
    /// forget the engine.
    pub fn take_queued_for_key(&self, key: &EngineKey, engine_id: &str) -> Vec<i64> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        inner.queue.retain(|q| {
            if &q.key == key {
                out.push(q.run_id);
                false
            } else {
                true
            }
        });
        inner.engines.remove(engine_id);
        out
    }

    pub fn engines_snapshot(&self) -> Vec<serde_json::Value> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .engines
            .values()
            .map(|e| {
                json!({
                    "id": e.id, "pid": e.pid, "module": e.key.module, "source_dir": e.key.source_dir,
                    "isolated": e.key.isolated, "nice": e.key.nice, "runs_done": e.runs_done, "current_run": e.current_run,
                    "adopted": e.adopted, "exit_requested": e.exit_requested,
                    "uptime_secs": e.spawned_at.elapsed().as_secs(),
                })
            })
            .collect()
    }

    /// PIDs of engines with no run in progress, for the shutdown backstop. An engine that is
    /// executing a run is deliberately left alone: `Restart adoption` requires it to outlive
    /// the server so a restarted one can adopt its run.
    pub fn idle_engine_pids(&self) -> Vec<u32> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .engines
            .values()
            .filter(|e| e.current_run.is_none())
            .filter_map(|e| e.pid)
            .collect()
    }

    pub fn queue_len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .queue
            .len()
    }

    /// Reap exited children and detect engines that died mid-run. Returns
    /// the runs that need a terminal state: (run_id, was_cancelling).
    fn sweep(&self) -> Vec<(i64, bool, String)> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut dead: Vec<String> = Vec::new();
        for e in inner.engines.values_mut() {
            let exited = match e.child.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                None => e.pid.map(|p| !process::is_alive(p)).unwrap_or(true),
            };
            if exited {
                dead.push(e.id.clone());
            }
        }
        let mut out = Vec::new();
        for id in dead {
            if let Some(e) = inner.engines.remove(&id) {
                if let Some(run_id) = e.current_run {
                    out.push((run_id, false, id));
                }
            }
        }
        out
    }
}

pub enum WorkDecision {
    Run(i64),
    Job(serde_json::Value),
    Exit,
    Wait,
}

/// Periodic supervision: reap engines, detect crashes, escalate cancels,
/// and keep the pool sized to the queue.
pub async fn monitor_loop(state: Arc<AppState>, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = shutdown.changed() => { if *shutdown.borrow() { return; } }
        }
        let st = state.clone();
        let _ = tokio::task::spawn_blocking(move || supervise_once(&st)).await;
    }
}

fn supervise_once(state: &Arc<AppState>) {
    let sup = &state.supervisor;
    // 1. Engines that exited.
    for (run_id, _, _engine) in sup.sweep() {
        if let Some(active) = state.index.get(run_id) {
            if active.state.is_terminal() {
                continue;
            }
            if active.cancel_requested {
                let _ = state.transition_run(
                    run_id,
                    State::new(StateType::Cancelled).with_message("cancelled"),
                    false,
                );
            } else {
                crate::dispatch::crash_run(state, run_id, "engine process exited unexpectedly");
            }
        }
    }
    // 2. Heartbeat timeouts for runs whose engine we do not own as a child.
    let heartbeat_limit = sup.heartbeat * 3;
    for active in state.index.active_runs() {
        if active.state.state_type != StateType::Running
            && active.state.state_type != StateType::Cancelling
        {
            continue;
        }
        let Some(pid) = active.engine_pid else {
            continue;
        };
        if active.last_heartbeat.elapsed() > heartbeat_limit && !process::is_alive(pid) {
            if active.cancel_requested {
                let _ = state.transition_run(
                    active.id,
                    State::new(StateType::Cancelled).with_message("cancelled"),
                    false,
                );
            } else {
                crate::dispatch::crash_run(state, active.id, "engine process exited unexpectedly");
            }
        }
    }
    // 2b. Runs waiting on a resource or an engine slot.
    for (run_id, reason) in sup.take_waiting_marks() {
        if let Some(active) = state.index.get(run_id) {
            if active.state.state_type == StateType::Scheduled && active.state.name != "Late" {
                let mut s = State::named(cereyan_core::StateName::AwaitingResource);
                s.message = Some(if reason == "no engine slot" {
                    reason.clone()
                } else {
                    format!("waiting for {reason}")
                });
                s.details
                    .insert("resource".into(), serde_json::Value::String(reason.clone()));
                if let Ok(crate::state::TransitionResult::Accepted(_)) =
                    state.transition_run(run_id, s, false)
                {
                    if reason != "no engine slot" {
                        let _ = state.record_event(
                            "resource.exhausted",
                            Some(run_id),
                            Some(active.flow_id),
                            json!({"resource": reason}),
                        );
                    }
                }
            }
        }
    }
    // 3. Cancellation escalation.
    for active in state.index.active_runs() {
        if active.state.state_type != StateType::Cancelling {
            continue;
        }
        let Some(since) = active.cancelling_since else {
            continue;
        };
        let Some(pid) = active.engine_pid else {
            let _ = state.transition_run(active.id, State::new(StateType::Cancelled), false);
            continue;
        };
        let elapsed = since.elapsed();
        if elapsed > sup.cancel_grace * 2 {
            process::kill(pid);
            let _ = state.transition_run(
                active.id,
                State::new(StateType::Cancelled).with_message("killed"),
                false,
            );
        } else if elapsed > sup.cancel_grace && active.terminated_at.is_none() {
            process::terminate(pid);
            state
                .index
                .update(active.id, |r| r.terminated_at = Some(Instant::now()));
        }
    }
    // 4. Keep the pool sized to the queue.
    if sup.queue_len() > 0 {
        sup.ensure_capacity(state);
    }
}
