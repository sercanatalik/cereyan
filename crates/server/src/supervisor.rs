//! Engine supervisor: a warm pool of Python engine processes keyed by source
//! directory, module, and isolation; a pull-based work queue; heartbeat and
//! exit monitoring; cancellation escalation; restart adoption.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use cereyan_core::{EventName, Flow, Run, State, StateType};
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

/// The machine's CPU count: the most engines the pool may hold.
pub fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Niceness for a priority: negative priorities lower the OS priority.
pub fn nice_for(priority: i64) -> u8 {
    if priority < 0 {
        (-priority).min(19) as u8
    } else {
        0
    }
}

/// Where an engine runs: on the server's machine or on a registered worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    Local,
    Worker(i64),
}

impl Location {
    /// Engines a worker starts are named `w<worker id>-<n>` by the server, so
    /// their location survives a server restart; every other engine is local.
    pub fn of_engine(engine_id: &str) -> Location {
        engine_id
            .strip_prefix('w')
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(n, _)| n.parse().ok())
            .map(Location::Worker)
            .unwrap_or(Location::Local)
    }
}

impl EngineKey {
    /// The same code wherever it sits: module, isolation and niceness, not the
    /// directory, which differs between a worker's checkout and the server's.
    pub fn same_code(&self, other: &EngineKey) -> bool {
        self.module == other.module && self.isolated == other.isolated && self.nice == other.nice
    }

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
    /// Set when the pool shrank below this busy engine: it finishes its run, then exits.
    pub drained_at: Option<Instant>,
    /// When the current run was handed over.
    pub run_since: Option<Instant>,
    /// Whether the engine has asked for work since it started.
    pub polled: bool,
    pub location: Location,
    /// The processor slot on its host, from 1.
    pub slot: usize,
}

impl Engine {
    fn new(
        id: String,
        key: EngineKey,
        pid: Option<u32>,
        child: Option<Child>,
        adopted: bool,
    ) -> Engine {
        Engine {
            id,
            module_mtime: key.module_mtime(),
            key,
            pid,
            child,
            runs_done: 0,
            current_run: None,
            exit_requested: false,
            spawned_at: Instant::now(),
            last_seen: Instant::now(),
            adopted,
            drained_at: None,
            run_since: None,
            polled: false,
            location: Location::Local,
            slot: 0,
        }
    }

    /// `starting`, `idle`, `running` or `draining`.
    pub fn status(&self) -> &'static str {
        match (self.current_run, self.drained_at, self.polled) {
            (Some(_), Some(_), _) => "draining",
            (Some(_), None, _) => "running",
            (None, _, false) => "starting",
            (None, _, true) => "idle",
        }
    }
}

/// One engine as the Queue page shows it.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct EngineView {
    pub id: String,
    /// `server`, or the worker's name.
    pub host: String,
    /// The processor slot on that host, from 1.
    pub slot: usize,
    /// `starting`, `idle`, `running` or `draining`.
    pub status: &'static str,
    /// The module the engine has loaded.
    pub module: String,
    pub run_id: Option<i64>,
    /// Seconds in the current run, or since the engine started when it has none.
    pub since_secs: u64,
}

/// One queued run that is due, in dispatch order.
#[derive(Clone, Debug)]
pub struct LineEntry {
    pub run_id: i64,
    pub module: String,
    pub priority: i64,
    /// 1 for the first run in line.
    pub position: usize,
    /// Scheduled or creation time (microseconds): when it joined the line.
    pub order: i64,
    pub can_start: bool,
    pub reason: Option<String>,
    pub overtaken_by: u32,
}

/// How long a worker has to start an engine it was asked for.
const REMOTE_START_TIMEOUT: Duration = Duration::from_secs(60);
/// A worker's idle engine long-polls every 25 s; after this long without a
/// poll it is gone.
const REMOTE_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// A queued run's place in line: priority first (highest first), then time
/// in line, then id.
type QKey = (i64, i64, i64);

fn qkey(q: &QueuedRun) -> QKey {
    (-q.priority, q.order, q.run_id)
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
    /// The run's flow, for matching a worker's code.
    pub flow_id: i64,
    /// Whether a remote worker may take it (`runs_on` is not `server`).
    pub remote_ok: bool,
    /// A replay of a run last executed on this worker waits for it until
    /// `prefer_until` (microseconds), then any host may take it.
    pub prefer_worker: Option<i64>,
    pub prefer_until: i64,
}

impl QueuedRun {
    /// Whether the run is still reserved for another host than `location`.
    fn reserved_elsewhere(&self, location: Location, now: i64) -> bool {
        match self.prefer_worker {
            Some(w) if now < self.prefer_until => location != Location::Worker(w),
            _ => false,
        }
    }
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

/// Whether `pattern` (with `*` matching any run of characters) matches `name`.
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(b'*') => (0..=n.len()).any(|i| go(&p[1..], &n[i..])),
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

/// Keyed instances at zero usage are dropped past this many.
const IDLE_INSTANCES_KEPT: usize = 256;

impl Resources {
    /// The explicit total, else the first pattern total (`api:*`) that matches,
    /// else 1.
    pub fn total(&self, name: &str) -> f64 {
        if let Some(t) = self.totals.get(name) {
            return *t;
        }
        self.pattern_for(name)
            .and_then(|p| self.totals.get(p))
            .copied()
            .unwrap_or(1.0)
    }

    /// The pattern total that gives `name` its total, when it has no explicit one.
    pub fn pattern_for(&self, name: &str) -> Option<&str> {
        if self.totals.contains_key(name) {
            return None;
        }
        let mut patterns: Vec<&String> = self.totals.keys().filter(|k| k.contains('*')).collect();
        patterns.sort();
        patterns
            .into_iter()
            .find(|p| glob_matches(p, name))
            .map(|p| p.as_str())
    }

    /// Whether a limit exists for `name` at all, explicit or by pattern.
    pub fn is_declared(&self, name: &str) -> bool {
        self.totals.contains_key(name) || self.pattern_for(name).is_some()
    }

    /// Drop keyed instances nobody holds once there are many of them.
    fn evict_idle(&mut self) {
        let idle: Vec<String> = self
            .used
            .iter()
            .filter(|(n, u)| **u <= 1e-9 && !self.totals.contains_key(*n))
            .map(|(n, _)| n.clone())
            .collect();
        if idle.len() > IDLE_INSTANCES_KEPT {
            for n in idle {
                self.used.remove(&n);
            }
        }
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
        self.evict_idle();
    }
    pub fn release_all(&mut self, run_id: i64) {
        if let Some(list) = self.leases.remove(&run_id) {
            for (_, n, a) in list {
                if let Some(u) = self.used.get_mut(&n) {
                    *u = (*u - a).max(0.0);
                }
            }
        }
        self.evict_idle();
    }
}

/// What the supervisor knows of a registered worker.
#[derive(Debug)]
pub struct WorkerSlot {
    pub name: String,
    pub processors: usize,
    /// `online`, `draining`, or `offline`.
    pub state: String,
    /// Flows whose code on the worker matches the server's.
    pub eligible: std::collections::HashSet<i64>,
    pub last_seen: Instant,
    /// Commands for the worker, handed over on its next heartbeat.
    pub commands: Vec<serde_json::Value>,
    /// Modules its engines failed to import; skipped until its checkout changes.
    pub broken: std::collections::HashSet<String>,
    next_engine: u64,
}

#[derive(Default)]
struct Inner {
    engines: HashMap<String, Engine>,
    workers: HashMap<i64, WorkerSlot>,
    /// The one queue, in dispatch order.
    queue: BTreeMap<QKey, QueuedRun>,
    /// Each queued run's place in `queue`.
    positions: HashMap<i64, QKey>,
    /// How many later runs were dispatched while a queued run could not start.
    overtaken: HashMap<i64, u32>,
    jobs: VecDeque<QueuedJob>,
    next_engine: u64,
    /// Runs already reported as waiting for a resource (avoid repeated transitions).
    waiting_marked: HashMap<i64, String>,
    failures: HashMap<i64, Vec<i64>>,
}

impl Inner {
    fn push(&mut self, item: QueuedRun) {
        if self.positions.contains_key(&item.run_id) {
            return;
        }
        let k = qkey(&item);
        self.positions.insert(item.run_id, k);
        self.queue.insert(k, item);
    }

    fn remove(&mut self, run_id: i64) -> Option<QueuedRun> {
        self.overtaken.remove(&run_id);
        let k = self.positions.remove(&run_id)?;
        self.queue.remove(&k)
    }

    /// Engines counted against the pool: every one but those told to exit
    /// while idle (they are on their way out).
    fn occupying(&self) -> usize {
        self.occupying_at(Location::Local)
    }

    fn occupying_at(&self, location: Location) -> usize {
        self.engines
            .values()
            .filter(|e| e.location == location && (!e.exit_requested || e.current_run.is_some()))
            .count()
    }

    /// The lowest processor slot not taken at `location`.
    fn free_slot(&self, location: Location) -> usize {
        let taken: std::collections::HashSet<usize> = self
            .engines
            .values()
            .filter(|e| e.location == location)
            .map(|e| e.slot)
            .collect();
        (1..).find(|n| !taken.contains(n)).unwrap_or(1)
    }
}

pub struct Supervisor {
    inner: Mutex<Inner>,
    pub resources: Mutex<Resources>,
    pub notify: Notify,
    python: String,
    home: std::path::PathBuf,
    token: Option<String>,
    max_engines: AtomicUsize,
    /// The most `max_engines` may be: the machine's CPU count.
    pub cpu_cap: usize,
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
    /// Which execution of the run's body this is: 0 the first time, the next
    /// after a resume. The engine counts its own retries up from here and
    /// reports it with every task run it creates.
    #[serde(default)]
    pub pass: i64,
    /// Checkpoints a new attempt may replay: the run's earlier passes and, for a
    /// crash rerun, the chain of crashed runs before it.
    #[serde(default)]
    pub checkpoints: Vec<cereyan_store::Checkpoint>,
    /// A forced (restated) run: the engine ignores targets, caches and checkpoints.
    #[serde(default)]
    pub force: bool,
    /// The last report sequence the store recorded for this run. A fresh engine
    /// process buffers from zero, and the store skips any event at or below the
    /// run's sequence as a redelivery, so an engine picking up a run someone
    /// else already reported on has to continue that count rather than restart
    /// it. Without this a resumed run's whole report was silently dropped.
    #[serde(default)]
    pub report_seq: i64,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
    /// The hand-off this execution belongs to. The engine sends it back as
    /// `X-Cereyan-Lease` on reports, heartbeats and transitions; an older lease
    /// is refused, so a run rerun elsewhere is not also finished here.
    #[serde(default)]
    pub lease: i64,
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
        let cpu_cap = cpu_count();
        if config.max_engines > cpu_cap {
            eprintln!(
                "cereyan: max_engines {} is above this machine's {cpu_cap} CPUs; using {cpu_cap}",
                config.max_engines
            );
        }
        Supervisor {
            inner: Mutex::new(Inner::default()),
            resources: Mutex::new(resources),
            notify: Notify::new(),
            python: config.python.clone(),
            home: config.home.clone(),
            token: config.token.clone(),
            max_engines: AtomicUsize::new(config.max_engines.clamp(1, cpu_cap)),
            cpu_cap,
            engine_max_runs: config.engine_max_runs.max(1),
            cancel_grace: Duration::from_secs(config.cancel_grace_secs.max(1)),
            heartbeat: Duration::from_secs(config.heartbeat_secs.max(1)),
        }
    }

    /// The pool size in force: how many engines may run at once.
    pub fn max_engines(&self) -> usize {
        self.max_engines.load(Ordering::Relaxed)
    }

    /// Resize the pool while the server runs. Growing lets `ensure_capacity`
    /// start engines up to `n`, cancelling drains first. Shrinking tells idle
    /// engines above `n` to exit and drains busy ones, youngest run first: a
    /// draining engine finishes its run and then exits, so no run is
    /// interrupted. `n` is clamped to `1..=cpu_cap`; callers validate first.
    pub fn set_max_engines(&self, n: usize) {
        let n = n.clamp(1, self.cpu_cap);
        self.max_engines.store(n, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut live = inner
            .engines
            .values()
            .filter(|e| e.location == Location::Local && !e.exit_requested)
            .count();
        // Growing: undo drains, most recent first.
        while live < n {
            let undo = inner
                .engines
                .values_mut()
                .filter(|e| {
                    e.location == Location::Local
                        && e.drained_at.is_some()
                        && e.current_run.is_some()
                })
                .max_by_key(|e| e.drained_at);
            match undo {
                Some(e) => {
                    e.drained_at = None;
                    e.exit_requested = false;
                    live += 1;
                }
                None => break,
            }
        }
        // Shrinking: idle engines first, then busy ones, youngest run first.
        let mut live: Vec<&mut Engine> = inner
            .engines
            .values_mut()
            .filter(|e| e.location == Location::Local && !e.exit_requested)
            .collect();
        if live.len() > n {
            let excess = live.len() - n;
            live.sort_by_key(|e| {
                (
                    e.current_run.is_some(),
                    std::cmp::Reverse(e.run_since.unwrap_or(e.spawned_at)),
                )
            });
            for e in live.into_iter().take(excess) {
                e.exit_requested = true;
                if e.current_run.is_some() {
                    e.drained_at = Some(Instant::now());
                }
            }
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    // ---- workers -----------------------------------------------------------

    /// Learn or refresh a worker: its name, processors, state, and the flows
    /// whose code matches the server's.
    pub fn sync_worker(
        &self,
        worker_id: i64,
        name: &str,
        processors: usize,
        state: &str,
        eligible: std::collections::HashSet<i64>,
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let slot = inner
            .workers
            .entry(worker_id)
            .or_insert_with(|| WorkerSlot {
                name: name.to_string(),
                processors,
                state: state.to_string(),
                eligible: Default::default(),
                last_seen: Instant::now(),
                commands: Vec::new(),
                broken: Default::default(),
                next_engine: 0,
            });
        slot.name = name.to_string();
        slot.processors = processors.max(1);
        slot.state = state.to_string();
        slot.eligible = eligible;
        slot.last_seen = Instant::now();
        drop(inner);
        self.notify.notify_waiters();
    }

    /// A worker's heartbeat: it is alive, and these are the engines it runs.
    /// Engines the server expects there but the worker no longer has are
    /// forgotten (their runs, if any, are left to the runs' own heartbeats).
    /// Returns the commands waiting for the worker, or `None` when the server
    /// does not know it (it should register again).
    pub fn worker_heartbeat(
        &self,
        worker_id: i64,
        engines: &[String],
    ) -> Option<Vec<serde_json::Value>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let slot = inner.workers.get_mut(&worker_id)?;
        slot.last_seen = Instant::now();
        if slot.state == "offline" {
            slot.state = "online".into();
        }
        let commands = std::mem::take(&mut slot.commands);
        let location = Location::Worker(worker_id);
        let reported: std::collections::HashSet<&String> = engines.iter().collect();
        let gone: Vec<String> = inner
            .engines
            .values()
            .filter(|e| {
                e.location == location
                    && e.current_run.is_none()
                    && !reported.contains(&e.id)
                    && (e.polled || e.spawned_at.elapsed() > Duration::from_secs(15))
            })
            .map(|e| e.id.clone())
            .collect();
        for id in gone {
            inner.engines.remove(&id);
        }
        drop(inner);
        self.notify.notify_waiters();
        Some(commands)
    }

    /// `online`, `draining`, or `offline`. A worker that is not online takes
    /// no new run: its idle engines are told to exit and engines it was asked
    /// to start but has not are forgotten.
    pub fn set_worker_state(&self, worker_id: i64, state: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = inner.workers.get_mut(&worker_id) else {
            return;
        };
        slot.state = state.to_string();
        if state != "online" {
            slot.commands.retain(|c| c["cmd"] != "spawn");
            let location = Location::Worker(worker_id);
            inner
                .engines
                .retain(|_, e| e.location != location || e.polled || e.current_run.is_some());
            for e in inner.engines.values_mut() {
                if e.location == location && e.current_run.is_none() {
                    e.exit_requested = true;
                }
            }
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    pub fn set_worker_processors(&self, worker_id: i64, processors: usize) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = inner.workers.get_mut(&worker_id) {
            slot.processors = processors.max(1);
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    /// Forget a worker and every engine it had.
    pub fn forget_worker(&self, worker_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.workers.remove(&worker_id);
        let location = Location::Worker(worker_id);
        inner.engines.retain(|_, e| e.location != location);
    }

    /// A worker's engine could not import `module`: stop sending it that
    /// module's runs until the worker reports a changed checkout.
    pub fn mark_broken(&self, worker_id: i64, module: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = inner.workers.get_mut(&worker_id) {
            slot.broken.insert(module.to_string());
        }
    }

    /// The worker reported new fingerprints: give its modules another chance.
    pub fn clear_broken(&self, worker_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = inner.workers.get_mut(&worker_id) {
            slot.broken.clear();
        }
    }

    /// Queue a command for a worker's next heartbeat.
    pub fn command_worker(&self, worker_id: i64, command: serde_json::Value) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = inner.workers.get_mut(&worker_id) {
            slot.commands.push(command);
        }
    }

    /// Workers not heard from within `timeout`: they turn offline, and their
    /// idle engines are forgotten. Returns the ids that changed.
    pub fn mark_quiet_workers(&self, timeout: Duration) -> Vec<i64> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let quiet: Vec<i64> = inner
            .workers
            .iter()
            .filter(|(_, s)| s.state != "offline" && s.last_seen.elapsed() > timeout)
            .map(|(id, _)| *id)
            .collect();
        for id in &quiet {
            if let Some(slot) = inner.workers.get_mut(id) {
                slot.state = "offline".into();
                slot.commands.clear();
            }
            let location = Location::Worker(*id);
            inner
                .engines
                .retain(|_, e| e.location != location || e.current_run.is_some());
        }
        quiet
    }

    /// The worker whose engine holds `run_id`, if a worker's does.
    pub fn run_worker(&self, run_id: i64) -> Option<i64> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .engines
            .values()
            .find_map(|e| match (e.current_run, e.location) {
                (Some(r), Location::Worker(w)) if r == run_id => Some(w),
                _ => None,
            })
    }

    /// The engine holding `run_id`: its location and processor slot.
    pub fn run_placement(&self, run_id: i64) -> Option<(Location, usize)> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .engines
            .values()
            .find(|e| e.current_run == Some(run_id))
            .map(|e| (e.location, e.slot))
    }

    /// Per worker: (id, name, state, processors, running, idle or starting).
    pub fn workers_snapshot(&self) -> Vec<(i64, String, String, usize, usize, usize)> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<_> = inner
            .workers
            .iter()
            .map(|(id, s)| {
                let location = Location::Worker(*id);
                let mine = inner.engines.values().filter(|e| e.location == location);
                let running = mine.clone().filter(|e| e.current_run.is_some()).count();
                let idle = mine.filter(|e| e.current_run.is_none()).count();
                (
                    *id,
                    s.name.clone(),
                    s.state.clone(),
                    s.processors,
                    running,
                    idle,
                )
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    /// The worker name for an id the supervisor knows.
    pub fn worker_name(&self, worker_id: i64) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.workers.get(&worker_id).map(|s| s.name.clone())
    }

    pub fn enqueue(&self, item: QueuedRun) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.push(item);
        drop(inner);
        self.notify.notify_waiters();
    }

    /// Queue many runs at once (backfills), notifying waiters once.
    pub fn enqueue_many(&self, items: Vec<QueuedRun>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for item in items {
            inner.push(item);
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    /// Convenience for callers that only know the run and key.
    pub fn enqueue_simple(&self, run_id: i64, flow_id: i64, remote_ok: bool, key: EngineKey) {
        self.enqueue(QueuedRun {
            run_id,
            key,
            priority: 0,
            order: cereyan_core::now_micros(),
            needs: Vec::new(),
            not_before: None,
            flow_id,
            remote_ok,
            prefer_worker: None,
            prefer_until: 0,
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
                    let mut entry = serde_json::json!({"total": r.total(n), "used": r.used.get(n).copied().unwrap_or(0.0)});
                    if let Some(p) = r.pattern_for(n) {
                        entry["pattern"] = serde_json::Value::String(p.to_string());
                    }
                    (n.clone(), entry)
                })
                .collect(),
        )
    }

    /// Whether a total is declared for `name`, explicitly or by pattern.
    pub fn resource_declared(&self, name: &str) -> bool {
        self.resources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_declared(name)
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

    pub fn forget_engine(&self, engine_id: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.engines.remove(engine_id);
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
        let queued: Vec<QueuedRun> = inner.queue.values().cloned().collect();
        let engines_full = inner.occupying() >= self.max_engines()
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
        inner.waiting_marked.remove(&run_id);
        inner.remove(run_id).is_some()
    }

    pub fn dequeue_many(&self, run_ids: &[i64]) {
        let set: std::collections::HashSet<i64> = run_ids.iter().copied().collect();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for id in &set {
            inner.remove(*id);
            inner.waiting_marked.remove(id);
        }
    }

    pub fn queued(&self, run_id: i64) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .positions
            .contains_key(&run_id)
    }

    /// Track an engine that a previous server started.
    pub fn adopt(&self, run: &Run, key: EngineKey) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = run
            .engine_id
            .clone()
            .unwrap_or_else(|| format!("adopted-{}", run.engine_pid.unwrap_or(0)));
        let mut engine = Engine::new(
            id.clone(),
            key,
            run.engine_pid.map(|p| p as u32),
            None,
            true,
        );
        engine.module_mtime = None;
        engine.current_run = Some(run.id);
        engine.run_since = Some(Instant::now());
        engine.polled = true;
        engine.location = Location::of_engine(&id);
        engine.slot = run
            .processor
            .map(|p| p.max(1) as usize)
            .unwrap_or_else(|| inner.free_slot(engine.location));
        inner.engines.insert(id, engine);
    }

    /// Spawn engines so that every queued run has an engine that can take it,
    /// within `max_engines`. Idle engines of other keys are asked to exit
    /// when the pool is full.
    pub fn ensure_capacity(&self, _state: &AppState) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Demand per key: how many runs could start, and which of their flows
        // may go to a worker.
        let mut demand: HashMap<EngineKey, (usize, Vec<i64>)> = HashMap::new();
        let mut preferred: Vec<(EngineKey, i64)> = Vec::new();
        let horizon = cereyan_core::now_micros() + crate::scheduler::PREWARM_SECS * 1_000_000;
        {
            let resources = self.resources.lock().unwrap_or_else(|e| e.into_inner());
            for q in inner.queue.values() {
                if q.not_before.map(|t| t > horizon).unwrap_or(false) {
                    continue;
                }
                if resources.can_acquire(&q.needs).is_some() {
                    continue;
                }
                if let Some(w) = q
                    .prefer_worker
                    .filter(|_| q.prefer_until > cereyan_core::now_micros())
                {
                    preferred.push((q.key.clone(), w));
                    continue;
                }
                let entry = demand.entry(q.key.clone()).or_default();
                entry.0 += 1;
                if q.remote_ok {
                    entry.1.push(q.flow_id);
                }
            }
        }
        for j in &inner.jobs {
            demand.entry(j.key.clone()).or_default().0 += 1;
        }
        for (key, (pending, remote_flows)) in demand {
            let usable = inner
                .engines
                .values()
                .filter(|e| {
                    e.location == Location::Local
                        && e.key == key
                        && !e.exit_requested
                        && e.current_run.is_none()
                })
                .count();
            let mut to_spawn = pending.saturating_sub(usable);
            // The server's own processors first: they are warm and nearest.
            while to_spawn > 0 {
                if inner.occupying() >= self.max_engines() {
                    // Evict one idle local engine of another key.
                    let victim = inner.engines.values_mut().find(|e| {
                        e.location == Location::Local
                            && e.key != key
                            && e.current_run.is_none()
                            && !e.exit_requested
                    });
                    if let Some(v) = victim {
                        v.exit_requested = true;
                    }
                    break;
                }
                let id = format!("engine-{}-{}", std::process::id(), inner.next_engine);
                inner.next_engine += 1;
                match self.spawn(&id, &key) {
                    Ok(child) => {
                        let pid = Some(child.id());
                        let mut engine =
                            Engine::new(id.clone(), key.clone(), pid, Some(child), false);
                        engine.slot = inner.free_slot(Location::Local);
                        inner.engines.insert(id, engine);
                    }
                    Err(e) => {
                        eprintln!("cereyan: failed to start engine: {e}");
                        break;
                    }
                }
                to_spawn -= 1;
            }
            // What the server cannot take now spills over to workers.
            if to_spawn > 0 && !remote_flows.is_empty() {
                Self::spill_over(&mut inner, &key, to_spawn, &remote_flows);
            }
        }
        // Replays reserved for the worker that ran them before.
        for (key, worker_id) in preferred {
            Self::spawn_on(&mut inner, &key, worker_id);
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    /// Make sure `worker_id` has an engine for `key` idle or starting, asking it
    /// to start one when it has a free processor.
    fn spawn_on(inner: &mut Inner, key: &EngineKey, worker_id: i64) {
        let location = Location::Worker(worker_id);
        let ready = inner.engines.values().any(|e| {
            e.location == location
                && e.key.same_code(key)
                && e.current_run.is_none()
                && !e.exit_requested
        });
        let busy = inner.occupying_at(location);
        let Some(slot) = inner.workers.get(&worker_id) else {
            return;
        };
        if ready
            || slot.state != "online"
            || slot.broken.contains(&key.module)
            || busy >= slot.processors
        {
            return;
        }
        let slot_no = inner.free_slot(location);
        let slot = inner.workers.get_mut(&worker_id).expect("checked above");
        slot.next_engine += 1;
        let engine_id = format!("w{worker_id}-{}", slot.next_engine);
        slot.commands.push(json!({
            "cmd": "spawn", "engine_id": engine_id, "module": key.module,
            "isolated": key.isolated, "nice": key.nice,
        }));
        let mut engine = Engine::new(engine_id.clone(), key.clone(), None, None, false);
        engine.module_mtime = None;
        engine.location = location;
        engine.slot = slot_no;
        inner.engines.insert(engine_id, engine);
    }

    /// The id of the worker named `name`, when the supervisor knows it.
    pub fn worker_id(&self, name: &str) -> Option<i64> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .workers
            .iter()
            .find(|(_, s)| s.name == name)
            .map(|(id, _)| *id)
    }

    /// Ask workers to start engines for `wanted` runs of `key`. Idle engines a
    /// worker already has for the code count first; then the eligible worker
    /// with the most free processors is asked, one engine at a time.
    fn spill_over(inner: &mut Inner, key: &EngineKey, wanted: usize, flows: &[i64]) {
        let eligible_for = |slot: &WorkerSlot| {
            slot.state == "online"
                && !slot.broken.contains(&key.module)
                && flows.iter().any(|f| slot.eligible.contains(f))
        };
        let idle_remote = inner
            .engines
            .values()
            .filter(|e| {
                matches!(e.location, Location::Worker(w)
                    if inner.workers.get(&w).is_some_and(&eligible_for))
                    && e.key.same_code(key)
                    && !e.exit_requested
                    && e.current_run.is_none()
            })
            .count();
        let mut wanted = wanted.saturating_sub(idle_remote);
        while wanted > 0 {
            let choice = inner
                .workers
                .iter()
                .filter(|(_, slot)| eligible_for(slot))
                .map(|(id, slot)| {
                    let busy = inner.occupying_at(Location::Worker(*id));
                    (*id, slot.processors.saturating_sub(busy))
                })
                .filter(|(_, free)| *free > 0)
                .max_by_key(|(id, free)| (*free, -*id));
            let Some((worker_id, _)) = choice else { break };
            let location = Location::Worker(worker_id);
            let slot_no = inner.free_slot(location);
            let slot = inner.workers.get_mut(&worker_id).expect("chosen above");
            slot.next_engine += 1;
            let engine_id = format!("w{worker_id}-{}", slot.next_engine);
            slot.commands.push(json!({
                "cmd": "spawn", "engine_id": engine_id, "module": key.module,
                "isolated": key.isolated, "nice": key.nice,
            }));
            let mut engine = Engine::new(engine_id.clone(), key.clone(), None, None, false);
            engine.module_mtime = None;
            engine.location = location;
            engine.slot = slot_no;
            inner.engines.insert(engine_id, engine);
            wanted -= 1;
        }
    }

    /// Spill-over alone, for tests: what `ensure_capacity` asks of workers
    /// when no local engine can be started.
    #[cfg(test)]
    fn spill_for_test(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut wanted: HashMap<EngineKey, Vec<i64>> = HashMap::new();
        for q in inner.queue.values().filter(|q| q.remote_ok) {
            wanted.entry(q.key.clone()).or_default().push(q.flow_id);
        }
        for (key, flows) in wanted {
            let n = flows.len();
            Self::spill_over(&mut inner, &key, n, &flows);
        }
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
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // The same intent on Windows, where a console control event otherwise
            // reaches every process sharing the server's group — including the
            // engine executing a run, which Restart adoption needs to outlive it.
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        cmd.spawn()
    }

    /// Work for an engine a worker started: the first run in line it may take.
    /// A remote engine takes a run only when the flow allows remote execution
    /// and the worker's code for it matches the server's. It never takes jobs
    /// (hooks, backfill prefilters: they run on the server), and it never
    /// gives up its slot for another module, since its worker has its own.
    fn take_remote_work(
        &self,
        inner: &mut Inner,
        engine_id: &str,
        pid: u32,
        key: &EngineKey,
        worker_id: i64,
    ) -> WorkDecision {
        let max_runs = self.engine_max_runs;
        let location = Location::Worker(worker_id);
        let occupying = inner.occupying_at(location);
        let Some(worker) = inner.workers.get(&worker_id) else {
            // A server that restarted learns the worker again on its next heartbeat.
            return WorkDecision::Wait;
        };
        let (processors, state) = (worker.processors, worker.state.clone());
        let engine = inner.engines.get_mut(engine_id).expect("tracked above");
        engine.last_seen = Instant::now();
        engine.pid = Some(pid);
        engine.polled = true;
        engine.current_run = None;
        engine.run_since = None;
        engine.drained_at = None;
        if engine.exit_requested
            || engine.runs_done >= max_runs
            || state != "online"
            || (occupying > processors && engine.adopted)
        {
            inner.engines.remove(engine_id);
            return WorkDecision::Exit;
        }
        let eligible = &inner.workers[&worker_id].eligible;
        let broken = &inner.workers[&worker_id].broken;
        let now = cereyan_core::now_micros();
        let mut resources = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        let mut blocked_ahead: Vec<i64> = Vec::new();
        let mut take: Option<QKey> = None;
        for (k, q) in inner.queue.iter() {
            if q.not_before.map(|t| t > now).unwrap_or(false)
                || !q.remote_ok
                || !eligible.contains(&q.flow_id)
                || !q.key.same_code(key)
                || broken.contains(&q.key.module)
                || q.reserved_elsewhere(Location::Worker(worker_id), now)
            {
                continue;
            }
            if resources.can_acquire(&q.needs).is_some() {
                blocked_ahead.push(q.run_id);
                continue;
            }
            take = Some(*k);
            break;
        }
        let Some(k) = take else {
            return WorkDecision::Wait;
        };
        let item = inner.queue.remove(&k).expect("key exists");
        inner.positions.remove(&item.run_id);
        inner.overtaken.remove(&item.run_id);
        for id in blocked_ahead {
            *inner.overtaken.entry(id).or_insert(0) += 1;
        }
        resources.acquire(item.run_id, &item.needs);
        inner.waiting_marked.remove(&item.run_id);
        if let Some(engine) = inner.engines.get_mut(engine_id) {
            engine.current_run = Some(item.run_id);
            engine.run_since = Some(Instant::now());
        }
        WorkDecision::Run(item.run_id)
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
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let inner = &mut *guard;
        let location = Location::of_engine(engine_id);
        let known = inner.engines.contains_key(engine_id);
        if !known {
            // An engine we did not spawn (previous server) or one that
            // reconnected after a restart: track it.
            let mut engine = Engine::new(engine_id.to_string(), key.clone(), Some(pid), None, true);
            engine.location = location;
            engine.slot = inner.free_slot(location);
            inner.engines.insert(engine_id.to_string(), engine);
        }
        if let Location::Worker(worker_id) = location {
            return self.take_remote_work(inner, engine_id, pid, key, worker_id);
        }
        let max_runs = self.engine_max_runs;
        let max_engines = self.max_engines();
        let over_capacity = inner.occupying() > max_engines;
        let engine = inner.engines.get_mut(engine_id).expect("just inserted");
        engine.last_seen = Instant::now();
        engine.pid = Some(pid);
        engine.polled = true;
        engine.current_run = None;
        engine.run_since = None;
        engine.drained_at = None;
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
        // The first run in line that can start goes to a free engine, whatever
        // its module. When it is another module's and nothing can serve it (no
        // idle engine of that module, no room to start one), this engine
        // exits to make that room rather than take a later run of its own.
        let now = cereyan_core::now_micros();
        let mut resources = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        let mut blocked_ahead: Vec<i64> = Vec::new();
        let mut take: Option<QKey> = None;
        let mut room = inner.occupying() < max_engines;
        let mut served_keys: Vec<&EngineKey> = Vec::new();
        for (k, q) in inner.queue.iter() {
            if q.not_before.map(|t| t > now).unwrap_or(false)
                || q.reserved_elsewhere(Location::Local, now)
            {
                continue;
            }
            if resources.can_acquire(&q.needs).is_some() {
                blocked_ahead.push(q.run_id);
                continue;
            }
            if &q.key == key {
                take = Some(*k);
                break;
            }
            if served_keys.contains(&&q.key) {
                continue;
            }
            let idle_other = inner.engines.values().any(|e| {
                e.id != engine_id && e.key == q.key && e.current_run.is_none() && !e.exit_requested
            });
            if idle_other {
                served_keys.push(&q.key);
                continue;
            }
            if room {
                // An engine for it can start; count it against the room.
                room = false;
                served_keys.push(&q.key);
                continue;
            }
            // Pool full and nothing can take that run: give up this slot.
            drop(resources);
            inner.engines.remove(engine_id);
            return WorkDecision::Exit;
        }
        let Some(k) = take else {
            return WorkDecision::Wait;
        };
        let item = inner.queue.remove(&k).expect("key exists");
        inner.positions.remove(&item.run_id);
        inner.overtaken.remove(&item.run_id);
        for id in blocked_ahead {
            *inner.overtaken.entry(id).or_insert(0) += 1;
        }
        resources.acquire(item.run_id, &item.needs);
        inner.waiting_marked.remove(&item.run_id);
        if let Some(engine) = inner.engines.get_mut(engine_id) {
            engine.current_run = Some(item.run_id);
            engine.run_since = Some(Instant::now());
        }
        WorkDecision::Run(item.run_id)
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
        inner.remove(run_id);
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
        let out: Vec<i64> = inner
            .queue
            .values()
            .filter(|q| &q.key == key)
            .map(|q| q.run_id)
            .collect();
        for id in &out {
            inner.remove(*id);
        }
        inner.engines.remove(engine_id);
        out
    }

    /// The pool and the queue as the Queue page shows them: each engine with
    /// its status, and the first `limit` queued runs in dispatch order with
    /// whether each can start now. Returns (engines, in line, total queued).
    pub fn queue_snapshot(&self, limit: usize) -> (Vec<EngineView>, Vec<LineEntry>, usize) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let resources = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        let now = cereyan_core::now_micros();
        let mut engines: Vec<EngineView> = inner
            .engines
            .values()
            .filter(|e| !e.exit_requested || e.current_run.is_some())
            .map(|e| EngineView {
                host: match e.location {
                    Location::Local => "server".to_string(),
                    Location::Worker(w) => inner
                        .workers
                        .get(&w)
                        .map(|slot| slot.name.clone())
                        .unwrap_or_else(|| format!("worker {w}")),
                },
                slot: e.slot,
                id: e.id.clone(),
                status: e.status(),
                module: e.key.module.clone(),
                run_id: e.current_run,
                since_secs: e.run_since.unwrap_or(e.spawned_at).elapsed().as_secs(),
            })
            .collect();
        engines.sort_by(|a, b| a.id.cmp(&b.id));
        let room = inner.occupying() < self.max_engines();
        let mut line = Vec::new();
        let mut position = 0;
        for q in inner.queue.values() {
            if q.not_before.map(|t| t > now).unwrap_or(false) {
                continue;
            }
            position += 1;
            if line.len() >= limit {
                continue;
            }
            let blocked = resources.can_acquire(&q.needs);
            let engine_free = room
                || inner
                    .engines
                    .values()
                    .any(|e| e.key == q.key && e.current_run.is_none() && !e.exit_requested);
            let reason = match &blocked {
                Some(name) if name.starts_with("flow:") => Some("max_concurrent".to_string()),
                Some(name) if name.starts_with("backfill:") => {
                    Some("backfill concurrency".to_string())
                }
                Some(name) => Some(format!("resource:{name}")),
                None if !engine_free
                    && q.remote_ok
                    && !inner.workers.is_empty()
                    && inner
                        .workers
                        .values()
                        .all(|w| !w.eligible.contains(&q.flow_id)) =>
                {
                    Some("no processor with matching code".to_string())
                }
                None if !engine_free => Some("no processor".to_string()),
                None => None,
            };
            line.push(LineEntry {
                run_id: q.run_id,
                module: q.key.module.clone(),
                priority: q.priority,
                position,
                order: q.order,
                can_start: reason.is_none(),
                reason,
                overtaken_by: inner.overtaken.get(&q.run_id).copied().unwrap_or(0),
            });
        }
        (engines, line, position)
    }

    /// Queued runs not yet due before `until` (microseconds), soonest first.
    pub fn joining(&self, until: i64, limit: usize) -> Vec<(i64, i64)> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = cereyan_core::now_micros();
        let mut out: Vec<(i64, i64)> = inner
            .queue
            .values()
            .filter_map(|q| {
                q.not_before
                    .filter(|t| *t > now && *t <= until)
                    .map(|t| (q.run_id, t))
            })
            .collect();
        out.sort_by_key(|(id, t)| (*t, *id));
        out.truncate(limit);
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
            // A worker's engine has no process here to check. One that never
            // started, or went quiet while idle, is forgotten; one holding a run
            // is left to the run's heartbeats.
            if e.location != Location::Local {
                let quiet = if e.polled {
                    e.current_run.is_none() && e.last_seen.elapsed() > REMOTE_IDLE_TIMEOUT
                } else {
                    e.spawned_at.elapsed() > REMOTE_START_TIMEOUT
                };
                if quiet {
                    dead.push(e.id.clone());
                }
                continue;
            }
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
        // A worker's engine is on another machine: its heartbeats are the only
        // sign of life, never a process check here.
        let remote = active
            .engine_id
            .as_deref()
            .is_some_and(|id| Location::of_engine(id) != Location::Local);
        if active.last_heartbeat.elapsed() > heartbeat_limit && (remote || !process::is_alive(pid))
        {
            if let Some(id) = active.engine_id.as_deref().filter(|_| remote) {
                sup.forget_engine(id);
            }
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
                        let _ = state.record_engine_event(
                            EventName::ResourceExhausted,
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
        // On a worker, the worker ends its own engine; signalling the pid here
        // would reach whatever process on this machine has the same number.
        if let Some(Location::Worker(worker_id)) =
            active.engine_id.as_deref().map(Location::of_engine)
        {
            if elapsed > sup.cancel_grace * 2 {
                let _ = state.transition_run(
                    active.id,
                    State::new(StateType::Cancelled)
                        .with_message("the worker did not stop the run in time"),
                    false,
                );
            } else if elapsed > sup.cancel_grace && active.terminated_at.is_none() {
                sup.command_worker(
                    worker_id,
                    json!({"cmd": "cancel", "run_id": active.id, "engine_id": active.engine_id}),
                );
                state
                    .index
                    .update(active.id, |r| r.terminated_at = Some(Instant::now()));
            }
            continue;
        }
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
    // 3b. Workers that stopped heartbeating turn offline.
    for worker_id in sup.mark_quiet_workers(sup.heartbeat * 3) {
        let _ = state.store.set_worker_state(worker_id, "offline");
        let _ = state.record_engine_event(
            EventName::WorkerOffline,
            None,
            None,
            json!({"worker_id": worker_id, "name": sup.worker_name(worker_id)}),
        );
    }
    // 4. Keep the pool sized to the queue.
    if sup.queue_len() > 0 {
        sup.ensure_capacity(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sup(max: usize) -> Supervisor {
        let config: ServeConfig = serde_json::from_value(json!({
            "home": std::env::temp_dir(),
            "max_engines": max,
        }))
        .unwrap();
        Supervisor::new(&config)
    }

    fn key(module: &str) -> EngineKey {
        EngineKey {
            source_dir: "/nonexistent".into(),
            module: module.into(),
            isolated: false,
            nice: 0,
        }
    }

    fn queued(run_id: i64, module: &str, priority: i64, order: i64) -> QueuedRun {
        QueuedRun {
            run_id,
            key: key(module),
            priority,
            order,
            needs: Vec::new(),
            not_before: None,
            flow_id: 1,
            remote_ok: true,
            prefer_worker: None,
            prefer_until: 0,
        }
    }

    fn take(s: &Supervisor, engine: &str, module: &str) -> Option<i64> {
        match s.take_work(engine, 1, &key(module), "") {
            WorkDecision::Run(id) => Some(id),
            WorkDecision::Exit => Some(-1),
            _ => None,
        }
    }

    #[test]
    fn a_deep_backlog_does_not_starve_another_module() {
        if cpu_count() < 2 {
            return;
        }
        let s = sup(2);
        assert_eq!(take(&s, "a", "etl"), None);
        assert_eq!(take(&s, "b", "etl"), None);
        s.enqueue_many((1..=500).map(|i| queued(i, "etl", 0, i)).collect());
        assert_eq!(take(&s, "a", "etl"), Some(1));
        assert_eq!(take(&s, "b", "etl"), Some(2));
        s.enqueue(queued(900, "ml", 5, 1_000));
        s.run_finished(1);
        // The next free etl engine gives up its slot for the ml run ahead of it.
        assert_eq!(take(&s, "a", "etl"), Some(-1));
        // An ml engine started in its place takes the ml run.
        assert_eq!(take(&s, "m", "ml"), Some(900));
    }

    #[test]
    fn oldest_first_across_modules() {
        let s = sup(1);
        assert_eq!(take(&s, "a", "etl"), None);
        s.enqueue(queued(1, "etl", 0, 10));
        s.enqueue(queued(2, "ml", 0, 20));
        assert_eq!(take(&s, "a", "etl"), Some(1));
    }

    #[test]
    fn an_engine_does_not_leave_for_a_run_behind_its_own() {
        let s = sup(1);
        assert_eq!(take(&s, "a", "etl"), None);
        s.enqueue(queued(1, "etl", 0, 10));
        s.enqueue(queued(2, "ml", 0, 20));
        s.enqueue(queued(3, "etl", 0, 30));
        assert_eq!(take(&s, "a", "etl"), Some(1));
        s.run_finished(1);
        // ml is now first: with the pool full, the etl engine exits.
        assert_eq!(take(&s, "a", "etl"), Some(-1));
    }

    #[test]
    fn a_blocked_run_is_overtaken() {
        let s = sup(1);
        s.set_total("gpu", 1.0);
        s.try_acquire(500, &[("gpu".into(), 1.0)]).unwrap();
        let mut first = queued(1, "etl", 0, 10);
        first.needs = vec![("gpu".into(), 1.0)];
        s.enqueue(first);
        s.enqueue(queued(2, "etl", 0, 20));
        assert_eq!(take(&s, "a", "etl"), Some(2));
        let inner = s.inner.lock().unwrap();
        assert_eq!(inner.overtaken.get(&1), Some(&1));
        assert!(inner.positions.contains_key(&1));
    }

    #[test]
    fn shrinking_drains_busy_engines_and_growing_undoes_it() {
        if cpu_count() < 2 {
            return;
        }
        let s = sup(2);
        s.enqueue(queued(1, "etl", 0, 10));
        s.enqueue(queued(2, "etl", 0, 20));
        assert_eq!(take(&s, "a", "etl"), Some(1));
        assert_eq!(take(&s, "b", "etl"), Some(2));
        s.set_max_engines(1);
        let draining = |s: &Supervisor| {
            s.inner
                .lock()
                .unwrap()
                .engines
                .values()
                .filter(|e| e.status() == "draining")
                .map(|e| e.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(draining(&s), vec!["b".to_string()]);
        // Undo: the drain is cancelled, no new engine is needed.
        s.set_max_engines(2);
        assert!(draining(&s).is_empty());
        // Drain again, then let the run end: the engine exits instead of taking work.
        s.set_max_engines(1);
        s.enqueue(queued(3, "etl", 0, 30));
        s.run_finished(2);
        assert_eq!(take(&s, "b", "etl"), Some(-1));
        assert_eq!(s.inner.lock().unwrap().engines.len(), 1);
    }

    #[test]
    fn the_pool_is_capped_at_the_cpu_count() {
        let s = sup(10_000);
        assert_eq!(s.max_engines(), cpu_count());
        s.set_max_engines(0);
        assert_eq!(s.max_engines(), 1);
    }

    fn worker(s: &Supervisor, id: i64, processors: usize, flows: &[i64]) {
        s.sync_worker(
            id,
            &format!("w{id}"),
            processors,
            "online",
            flows.iter().copied().collect(),
        );
    }

    fn commands(s: &Supervisor, id: i64) -> Vec<serde_json::Value> {
        s.worker_heartbeat(id, &[]).unwrap_or_default()
    }

    #[test]
    fn a_remote_engine_takes_only_runs_it_may_and_whose_code_matches() {
        let s = sup(1);
        worker(&s, 7, 2, &[1]);
        let mut pinned = queued(1, "etl", 0, 10);
        pinned.remote_ok = false;
        s.enqueue(pinned);
        let mut drifted = queued(2, "etl", 0, 20);
        drifted.flow_id = 99;
        s.enqueue(drifted);
        s.enqueue(queued(3, "etl", 0, 30));
        // runs_on="server" and a flow whose code differs are passed over.
        assert_eq!(take(&s, "w7-1", "etl"), Some(3));
        // The server's own engine still takes the pinned run first.
        assert_eq!(take(&s, "a", "etl"), Some(1));
    }

    #[test]
    fn a_full_server_spills_over_to_a_worker_with_the_code() {
        let s = sup(1);
        worker(&s, 7, 2, &[1]);
        worker(&s, 8, 4, &[]);
        assert_eq!(take(&s, "a", "etl"), None);
        s.enqueue(queued(1, "etl", 0, 10));
        assert_eq!(take(&s, "a", "etl"), Some(1));
        s.enqueue(queued(2, "etl", 0, 20));
        s.spill_for_test();
        let sent = commands(&s, 7);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["cmd"], "spawn");
        assert_eq!(sent[0]["module"], "etl");
        let engine = sent[0]["engine_id"].as_str().unwrap().to_string();
        assert!(engine.starts_with("w7-"));
        // Worker 8 has no matching code, so nothing was asked of it.
        assert!(commands(&s, 8).is_empty());
        assert_eq!(take(&s, &engine, "etl"), Some(2));
    }

    #[test]
    fn a_draining_worker_takes_nothing_new() {
        let s = sup(1);
        worker(&s, 7, 2, &[1]);
        s.enqueue(queued(1, "etl", 0, 10));
        s.set_worker_state(7, "draining");
        assert_eq!(take(&s, "w7-1", "etl"), Some(-1));
        s.set_worker_state(7, "online");
        assert_eq!(take(&s, "w7-2", "etl"), Some(1));
    }

    #[test]
    fn a_quiet_worker_turns_offline() {
        let s = sup(1);
        worker(&s, 7, 2, &[1]);
        assert!(s.mark_quiet_workers(Duration::from_secs(60)).is_empty());
        assert_eq!(s.mark_quiet_workers(Duration::from_secs(0)), vec![7]);
        assert_eq!(s.workers_snapshot()[0].2, "offline");
        // A heartbeat brings it back.
        commands(&s, 7);
        assert_eq!(s.workers_snapshot()[0].2, "online");
    }

    #[test]
    fn a_replay_waits_for_the_worker_that_ran_it() {
        let s = sup(1);
        worker(&s, 7, 2, &[1]);
        let mut replay = queued(1, "etl", 0, 10);
        replay.prefer_worker = Some(7);
        replay.prefer_until = cereyan_core::now_micros() + 60_000_000;
        s.enqueue(replay);
        assert_eq!(take(&s, "a", "etl"), None, "the server leaves it for w7");
        assert_eq!(take(&s, "w7-1", "etl"), Some(1));
    }

    #[test]
    fn a_module_a_worker_cannot_import_is_not_sent_there_again() {
        let s = sup(1);
        worker(&s, 7, 2, &[1]);
        s.mark_broken(7, "etl");
        s.enqueue(queued(1, "etl", 0, 10));
        assert_eq!(take(&s, "w7-1", "etl"), None);
        s.clear_broken(7);
        assert_eq!(take(&s, "w7-1", "etl"), Some(1));
    }
}
