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

/// One resource's limits and usage. See [`Supervisor::resource_rows`].
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ResourceRow {
    /// The resource's name. Carried here because the metrics renderer labels its
    /// lines with it, but **not serialised into the entry**: the snapshot keys
    /// its object by name, so a `name` field inside the entry would be a wire
    /// change for the settings endpoint and the two MCP resource tools.
    #[serde(skip)]
    pub name: String,
    pub total: f64,
    pub used: f64,
    /// The declared pattern that matched this name, if any. A resource with its
    /// own exact total has none — it needs no pattern.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// A non-run job for an engine of a key (hooks, bulk_complete prefilter).
#[derive(Clone, Debug)]
pub struct QueuedJob {
    pub key: EngineKey,
    pub payload: serde_json::Value,
}

#[derive(Default)]
pub struct Resources {
    /// Declared limits, by exact name.
    ///
    /// Private so the pattern list below cannot fall out of step with it: every
    /// write goes through `insert_total`.
    totals: HashMap<String, f64>,
    /// The names in `totals` that contain `*`, sorted. Maintained on insert so
    /// `pattern_for` borrows and scans instead of rebuilding and sorting a
    /// vector on every call — which happens once per queued run.
    patterns: Vec<String>,
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

    /// Register a total, keeping the pattern list in step.
    ///
    /// Only a *new* pattern name changes the list's order, so updating the
    /// value of an existing pattern skips the re-sort.
    fn insert_total(&mut self, name: &str, total: f64) {
        let is_new = self.totals.insert(name.to_string(), total).is_none();
        if is_new && name.contains('*') {
            self.patterns.push(name.to_string());
            self.patterns.sort();
        }
    }

    /// The pattern total that gives `name` its total, when it has no explicit one.
    pub fn pattern_for(&self, name: &str) -> Option<&str> {
        if self.totals.contains_key(name) {
            return None;
        }
        // Sorted, so the first match is the same one the old per-call sort
        // would have picked.
        self.patterns
            .iter()
            .find(|p| glob_matches(p, name))
            .map(|p| p.as_str())
    }

    /// Whether a limit exists for `name` at all, explicit or by pattern.
    pub fn is_declared(&self, name: &str) -> bool {
        self.totals.contains_key(name) || self.pattern_for(name).is_some()
    }

    /// Drop keyed instances nobody holds once there are many of them.
    fn evict_idle(&mut self) {
        // Below the threshold there is nothing to do, so do not walk the table
        // or build a vector of cloned keys to discover that.
        if self.used.len() <= IDLE_INSTANCES_KEPT {
            return;
        }
        let idle = self
            .used
            .iter()
            .filter(|(n, u)| **u <= 1e-9 && !self.totals.contains_key(*n))
            .count();
        if idle > IDLE_INSTANCES_KEPT {
            // Split the struct so the closure can read `totals` while `used` is
            // mutably borrowed.
            let Resources { used, totals, .. } = self;
            used.retain(|n, u| *u > 1e-9 || totals.contains_key(n));
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
    /// Local engines queued for creation but not yet registered, by key.
    /// Creation happens with `inner` released, so without this the cap check
    /// and the usable count would both miss engines already on their way.
    starting: HashMap<EngineKey, usize>,
    /// Runs taken off the queue whose work item has not yet reached the
    /// engine. Kept so a failed or abandoned hand-off can put the run back in
    /// line instead of leaving it dequeued with its resources held.
    handoff: HashMap<i64, QueuedRun>,
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

    /// Keys that currently have an engine able to take a run right now.
    ///
    /// Built once per queue scan so the per-run test is a hash lookup instead
    /// of comparing an `EngineKey` — three `String` comparisons — against every
    /// engine. The predicate is exactly the one this replaces, including not
    /// filtering on location, so the answers are unchanged.
    fn idle_engine_keys(&self) -> std::collections::HashSet<&EngineKey> {
        self.engines
            .values()
            .filter(|e| e.current_run.is_none() && !e.exit_requested)
            .map(|e| &e.key)
            .collect()
    }

    /// Is there an idle engine for `key` other than `engine_id`? The work taker
    /// must not count the engine that is asking for work.
    ///
    /// The set short-circuits the common case: if no engine is idle for the key
    /// the answer is false without touching the engine table. Only when one is
    /// does the exact per-engine test run, which is bounded by the engine cap.
    fn has_idle_other(
        &self,
        idle_keys: &std::collections::HashSet<&EngineKey>,
        key: &EngineKey,
        engine_id: &str,
    ) -> bool {
        if !idle_keys.contains(key) {
            return false;
        }
        self.engines.values().any(|e| {
            e.id != engine_id && e.key == *key && e.current_run.is_none() && !e.exit_requested
        })
    }

    /// Flow ids that at least one worker is eligible for, so the per-run test for
    /// "no processor with matching code" is a lookup rather than a scan of every
    /// worker.
    fn worker_eligible_flows(&self) -> std::collections::HashSet<i64> {
        self.workers
            .values()
            .flat_map(|w| w.eligible.iter().copied())
            .collect()
    }

    /// Engines counted against the pool: every one but those told to exit
    /// while idle (they are on their way out).
    fn occupying(&self) -> usize {
        self.occupying_at(Location::Local) + self.starting.values().sum::<usize>()
    }

    /// Local engines of `key` that can take a run now or soon: idle, or still
    /// being created.
    fn usable_local(&self, key: &EngineKey) -> usize {
        let idle = self
            .engines
            .values()
            .filter(|e| {
                e.location == Location::Local
                    && e.key == *key
                    && !e.exit_requested
                    && e.current_run.is_none()
            })
            .count();
        idle + self.starting.get(key).copied().unwrap_or(0)
    }

    /// Queue a local engine for creation once `inner` is released.
    fn request_spawn(&mut self, key: &EngineKey, requests: &mut Vec<(String, EngineKey)>) {
        let id = format!("engine-{}-{}", std::process::id(), self.next_engine);
        self.next_engine += 1;
        *self.starting.entry(key.clone()).or_default() += 1;
        requests.push((id, key.clone()));
    }

    /// Drop the reservations of requests whose creation has finished,
    /// successfully or not.
    fn finish_starting<'a>(&mut self, keys: impl IntoIterator<Item = &'a EngineKey>) {
        for key in keys {
            if let Some(n) = self.starting.get_mut(key) {
                *n -= 1;
                if *n == 0 {
                    self.starting.remove(key);
                }
            }
        }
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
    /// Module mtimes by engine key, each with when it was read. `take_work`
    /// runs on every engine poll and every wake-up, so the stat is cached for
    /// `MTIME_TTL` and never taken under `inner`.
    mtimes: Mutex<HashMap<EngineKey, (Instant, Option<SystemTime>)>>,
}

/// How long a module's mtime is trusted before it is read again: an edit is
/// noticed within this long, rather than on the very next poll.
const MTIME_TTL: Duration = Duration::from_secs(1);

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
            resources.insert_total(name, *total);
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
            mtimes: Mutex::new(HashMap::new()),
        }
    }

    /// The module's mtime, read at most once per `MTIME_TTL` per key.
    fn module_mtime(&self, key: &EngineKey) -> Option<SystemTime> {
        if let Some((at, mtime)) = self
            .mtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
        {
            if at.elapsed() < MTIME_TTL {
                return *mtime;
            }
        }
        // The stat runs with no lock held.
        let mtime = key.module_mtime();
        self.mtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), (Instant::now(), mtime));
        mtime
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
            Self::take_offline(&mut inner, *id);
        }
        quiet
    }

    /// A worker said it is stopping: it turns offline now rather than after
    /// three missed heartbeats.
    pub fn worker_left(&self, worker_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Self::take_offline(&mut inner, worker_id);
    }

    /// Mark a worker offline and forget its idle engines. The caller holds `inner`.
    fn take_offline(inner: &mut Inner, worker_id: i64) {
        if let Some(slot) = inner.workers.get_mut(&worker_id) {
            slot.state = "offline".into();
            slot.commands.clear();
        }
        let location = Location::Worker(worker_id);
        inner
            .engines
            .retain(|_, e| e.location != location || e.current_run.is_some());
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

    /// One worker's engines, by slot: what its status page lists under Now.
    pub fn worker_engines(&self, worker_id: i64) -> Vec<EngineView> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let location = Location::Worker(worker_id);
        let host = inner
            .workers
            .get(&worker_id)
            .map(|slot| slot.name.clone())
            .unwrap_or_else(|| format!("worker {worker_id}"));
        let mut engines: Vec<EngineView> = inner
            .engines
            .values()
            .filter(|e| e.location == location && (!e.exit_requested || e.current_run.is_some()))
            .map(|e| EngineView {
                host: host.clone(),
                slot: e.slot,
                id: e.id.clone(),
                status: e.status(),
                module: e.key.module.clone(),
                run_id: e.current_run,
                since_secs: e.run_since.unwrap_or(e.spawned_at).elapsed().as_secs(),
            })
            .collect();
        engines.sort_by(|a, b| a.slot.cmp(&b.slot).then_with(|| a.id.cmp(&b.id)));
        engines
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
        r.insert_total(name, total);
    }

    pub fn set_totals(&self, totals: &HashMap<String, f64>) {
        let mut r = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        for (k, v) in totals {
            r.insert_total(k, *v);
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

    /// Every resource as a typed row: name, declared total, usage, and the
    /// declared pattern that matched, if any.
    ///
    /// For callers that want the *values* — the metrics renderer, which was
    /// building a `serde_json::Value` object and then reading each total back out
    /// of it by string key. That read-back ended in `unwrap_or(0.0)`, so a
    /// renamed or missing field reported a resource total of zero, which looks
    /// exactly like an idle resource.
    ///
    /// `pattern` is owned rather than borrowed because the row outlives the lock:
    /// `pattern_for` returns a `&str` from the guarded map.
    pub fn resource_rows(&self) -> Vec<ResourceRow> {
        let r = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<&String> = r.totals.keys().chain(r.used.keys()).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|n| ResourceRow {
                name: n.clone(),
                total: r.total(n),
                used: r.used.get(n).copied().unwrap_or(0.0),
                pattern: r.pattern_for(n).map(|p| p.to_string()),
            })
            .collect()
    }

    /// The same rows, as a JSON object keyed by resource name.
    ///
    /// For the three callers whose response *is* JSON — the settings endpoint and
    /// the two MCP resource tools. Built from [`Self::resource_rows`] rather than
    /// written out again, so the JSON endpoint cannot drift from the values the
    /// metrics report.
    pub fn resources_snapshot(&self) -> serde_json::Value {
        // Keyed by name, as before, with the entry serialised from the row so the
        // field names cannot drift from the ones the metrics read. A row is a
        // `f64`/`Option<String>` struct and always serialises, so this cannot fail.
        serde_json::Value::Object(
            self.resource_rows()
                .into_iter()
                .map(|r| {
                    let name = r.name.clone();
                    // `name` is `serde(skip)`, so the entry is {total, used} plus
                    // `pattern` when set -- exactly the shape the hand-built
                    // `json!` produced.
                    (
                        name,
                        serde_json::to_value(&r).unwrap_or(serde_json::Value::Null),
                    )
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
    ///
    /// Lock ordering invariant: `inner` is always acquired before `resources`.
    /// No code path may acquire `resources` then `inner` — that would deadlock.
    pub fn take_waiting_marks(&self) -> Vec<(i64, String)> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let resources = self.resources.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        let now = cereyan_core::now_micros();
        let engines_full = inner.occupying() >= self.max_engines()
            && inner
                .engines
                .values()
                .all(|e| e.current_run.is_some() || e.exit_requested);
        // Collect modifications first to avoid borrow conflict (iterating inner.queue
        // while mutating inner.waiting_marked).
        let idle_keys = inner.idle_engine_keys();
        let mut marks: Vec<(i64, String)> = Vec::new();
        for q in inner.queue.values() {
            if q.not_before.map(|t| t > now).unwrap_or(false) {
                continue;
            }
            let reason = match resources.can_acquire(&q.needs) {
                Some(name) => Some(name),
                None if engines_full && !idle_keys.contains(&q.key) => {
                    Some("no engine slot".to_string())
                }
                None => None,
            };
            if let Some(reason) = reason {
                if inner.waiting_marked.get(&q.run_id) != Some(&reason) {
                    marks.push((q.run_id, reason));
                }
            }
        }
        for (run_id, reason) in &marks {
            inner.waiting_marked.insert(*run_id, reason.clone());
            out.push((*run_id, reason.clone()));
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
        // Lock ordering invariant: `inner` before `resources` when both are needed.
        let mut demand: HashMap<EngineKey, (usize, Vec<i64>)> = HashMap::new();
        let mut preferred: Vec<(EngineKey, i64)> = Vec::new();
        let horizon = cereyan_core::now_micros() + crate::scheduler::PREWARM_SECS * 1_000_000;
        {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
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
        // Collect spawn requests under the lock, then spawn outside it.
        let mut spawn_requests: Vec<(String, EngineKey)> = Vec::new();
        // Engines queued by the post-spawn re-check, and the handles they
        // produce. Both are filled and drained with `inner` released.
        let mut respawn_requests: Vec<(String, EngineKey)> = Vec::new();
        let mut spill_requests: Vec<(EngineKey, usize, Vec<i64>)> = Vec::new();
        let mut evictions: Vec<String> = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            for j in &inner.jobs {
                demand.entry(j.key.clone()).or_default().0 += 1;
            }
            for (key, (pending, remote_flows)) in &demand {
                let key = key.clone();
                let pending = *pending;
                let remote_flows = remote_flows.clone();
                let usable = inner.usable_local(&key);
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
                            evictions.push(v.id.clone());
                        }
                        break;
                    }
                    inner.request_spawn(&key, &mut spawn_requests);
                    to_spawn -= 1;
                }
                // What the server cannot take now spills over to workers.
                if to_spawn > 0 && !remote_flows.is_empty() {
                    spill_requests.push((key, to_spawn, remote_flows));
                }
            }
        }
        // Spawn processes outside the lock so other threads can enqueue/dequeue.
        let spawn_keys: Vec<EngineKey> = spawn_requests.iter().map(|(_, k)| k.clone()).collect();
        let spawned = self.spawn_all(spawn_requests);
        // Re-acquire the lock to insert engine records and handle spill-over.
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.finish_starting(&spawn_keys);
            Self::register_engines(&mut inner, spawned);
            for (key, to_spawn, remote_flows) in spill_requests {
                Self::spill_over(&mut inner, &key, to_spawn, &remote_flows);
            }
            // Replays reserved for the worker that ran them before.
            for (key, worker_id) in preferred {
                Self::spawn_on(&mut inner, &key, worker_id);
            }
            // Re-check demand after spawning: other threads may have enqueued
            // more runs while we were spawning. Only spawn more if there is
            // still unmet demand and we haven't hit the engine cap.
            for (key, (pending, _)) in demand.iter() {
                let still_needed = pending.saturating_sub(inner.usable_local(key));
                if still_needed > 0 && inner.occupying() < self.max_engines() {
                    // Queue one more engine for this key; it is created after
                    // the lock is released, like the first round.
                    inner.request_spawn(key, &mut respawn_requests);
                }
            }
        }
        // The re-check found demand that appeared while we were spawning. Fork
        // and exec with the lock released, exactly as the first round does:
        // `spawn` starts a Python interpreter, and holding `inner` across that
        // blocks every other supervisor operation.
        let respawn_keys: Vec<EngineKey> =
            respawn_requests.iter().map(|(_, k)| k.clone()).collect();
        let respawned = self.spawn_all(respawn_requests);
        if !respawn_keys.is_empty() {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.finish_starting(&respawn_keys);
            Self::register_engines(&mut inner, respawned);
        }
        self.notify.notify_waiters();
    }

    /// Create every requested engine with `inner` released, returning the
    /// handles for registration. Forking a Python interpreter takes
    /// milliseconds; doing it under the lock would stall every other
    /// supervisor operation, so callers must invoke this outside their lock
    /// scope.
    fn spawn_all(
        &self,
        requests: Vec<(String, EngineKey)>,
    ) -> Vec<(String, EngineKey, std::process::Child)> {
        let mut spawned = Vec::with_capacity(requests.len());
        for (id, key) in requests {
            match self.spawn(&id, &key) {
                Ok(child) => spawned.push((id, key, child)),
                Err(e) => eprintln!("cereyan: failed to start engine: {e}"),
            }
        }
        spawned
    }

    /// Record freshly started engines. The caller holds `inner`.
    fn register_engines(inner: &mut Inner, spawned: Vec<(String, EngineKey, std::process::Child)>) {
        for (id, key, child) in spawned {
            let pid = Some(child.id());
            let mut engine = Engine::new(id.clone(), key.clone(), pid, Some(child), false);
            engine.slot = inner.free_slot(Location::Local);
            inner.engines.insert(id, engine);
        }
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
                    if inner.workers.get(&w).is_some_and(eligible_for))
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
        let run_id = item.run_id;
        inner.handoff.insert(run_id, item);
        WorkDecision::Run(run_id)
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
        let location = Location::of_engine(engine_id);
        // Read before taking `inner`: a filesystem stat must not hold up every
        // other engine poll and enqueue, least of all on a network mount.
        let mtime = if location == Location::Local {
            self.module_mtime(key)
        } else {
            None
        };
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let inner = &mut *guard;
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
        let stale = match (engine.module_mtime, mtime) {
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
        let mut served_keys: std::collections::HashSet<&EngineKey> =
            std::collections::HashSet::new();
        let idle_keys = inner.idle_engine_keys();
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
            if served_keys.contains(&q.key) {
                continue;
            }
            let idle_other = inner.has_idle_other(&idle_keys, &q.key, engine_id);
            if idle_other {
                served_keys.insert(&q.key);
                continue;
            }
            if room {
                // An engine for it can start; count it against the room.
                room = false;
                served_keys.insert(&q.key);
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
        let run_id = item.run_id;
        inner.handoff.insert(run_id, item);
        WorkDecision::Run(run_id)
    }

    /// The work item for a taken run reached its engine: nothing to undo.
    pub fn handoff_done(&self, run_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.handoff.remove(&run_id);
    }

    /// Undo `take_work` for a run whose work item never reached its engine:
    /// free the engine and the resources and put the run back in line.
    /// Returns whether the run was requeued.
    pub fn abort_handoff(&self, run_id: i64) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(item) = inner.handoff.remove(&run_id) else {
            return false;
        };
        for e in inner.engines.values_mut() {
            if e.current_run == Some(run_id) {
                e.current_run = None;
                e.run_since = None;
            }
        }
        inner.push(item);
        drop(inner);
        self.resources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release_all(run_id);
        self.notify.notify_waiters();
        true
    }

    pub fn run_finished(&self, run_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.handoff.remove(&run_id);
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
        // Lock ordering invariant: `inner` before `resources`.
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
        // Built once: the per-run tests below are lookups, not scans.
        let idle_keys = inner.idle_engine_keys();
        let eligible_flows = inner.worker_eligible_flows();
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
            let engine_free = room || idle_keys.contains(&q.key);
            let reason = match &blocked {
                Some(name) if name.starts_with("flow:") => Some("max_concurrent".to_string()),
                Some(name) if name.starts_with("backfill:") => {
                    Some("backfill concurrency".to_string())
                }
                Some(name) => Some(format!("resource:{name}")),
                None if !engine_free
                    && q.remote_ok
                    && !inner.workers.is_empty()
                    && !eligible_flows.contains(&q.flow_id) =>
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

    /// How many engines have a run in progress.
    ///
    /// For the metrics sampler, which wants this one number every five seconds.
    /// Counting under the lock rather than filtering a serialised snapshot keeps
    /// the hold on `inner` — contended by every enqueue, poll and hand-off — down
    /// to a counter increment instead of a JSON object per engine.
    ///
    /// The predicate is `current_run.is_some()`, which is what the sampler
    /// derived from `engines_snapshot`. It is deliberately not `occupying()`,
    /// which counts idle engines against the pool and answers a different
    /// question.
    pub fn engines_busy_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .engines
            .values()
            .filter(|e| e.current_run.is_some())
            .count()
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
    for active in state
        .index
        .active_runs_in(&[StateType::Running, StateType::Cancelling])
    {
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
    for active in state.index.active_runs_in(&[StateType::Cancelling]) {
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

    #[test]
    fn take_waiting_marks_does_not_deadlock_under_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let s = Arc::new(sup(2));
        // Enqueue some runs so take_waiting_marks has work to do.
        for i in 1..=10 {
            s.enqueue(queued(i, "etl", 0, i));
        }
        let mut handles = Vec::new();
        // Spawn threads that call take_waiting_marks (acquires resources then inner).
        for _ in 0..4 {
            let s = Arc::clone(&s);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _ = s.take_waiting_marks();
                }
            }));
        }
        // Spawn threads that enqueue (acquires inner only).
        for _ in 0..2 {
            let s = Arc::clone(&s);
            handles.push(thread::spawn(move || {
                for i in 100..200 {
                    s.enqueue(queued(i, "etl", 0, i));
                }
            }));
        }
        // Spawn threads that call try_acquire (acquires resources only).
        for _ in 0..2 {
            let s = Arc::clone(&s);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _ = s.try_acquire(9999, &[("gpu".into(), 1.0)]);
                }
            }));
        }
        for h in handles {
            h.join().expect("thread should not deadlock");
        }
    }

    /// Engines queued for creation count against the cap and as usable for
    /// their key until registered, so neither one pass nor a concurrent
    /// `ensure_capacity` can queue past `max_engines`.
    #[test]
    fn starting_engines_count_against_the_cap() {
        let s = sup(2);
        let mut inner = s.inner.lock().unwrap();
        let mut requests = Vec::new();
        let etl = key("etl");
        while inner.occupying() < s.max_engines() {
            inner.request_spawn(&etl, &mut requests);
        }
        assert_eq!(
            requests.len(),
            2,
            "the cap stops the pass at two queued engines"
        );
        assert_eq!(inner.usable_local(&etl), 2);
        assert_eq!(inner.usable_local(&key("ml")), 0);
        let keys: Vec<EngineKey> = requests.iter().map(|(_, k)| k.clone()).collect();
        inner.finish_starting(&keys);
        assert_eq!(
            inner.occupying(),
            0,
            "a failed or registered start frees its reservation"
        );
        assert!(inner.starting.is_empty());
    }

    /// `spawn_all` must never take `inner` itself.
    ///
    /// The test is deterministic rather than a race: the main thread holds
    /// `inner` for the whole duration, and a worker calls `spawn_all`. If
    /// `spawn_all` acquired the lock it could not finish until the main thread
    /// released it, so observing the worker complete *while the lock is still
    /// held* proves the spawn path never needs the supervisor lock.
    ///
    /// The engine module points at a nonexistent directory, so `spawn` fails
    /// fast without starting a real interpreter. The lock discipline is the same
    /// on the success and failure paths.
    #[test]
    fn spawn_all_does_not_take_the_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let s = Arc::new(sup(4));
        let finished = Arc::new(AtomicBool::new(false));

        // Hold the supervisor lock for the whole test.
        let held = s.inner.lock().unwrap_or_else(|e| e.into_inner());

        let worker = {
            let s = Arc::clone(&s);
            let finished = Arc::clone(&finished);
            thread::spawn(move || {
                let requests: Vec<(String, EngineKey)> = (0..4)
                    .map(|i| (format!("engine-test-{i}"), key("etl")))
                    .collect();
                s.spawn_all(requests);
                finished.store(true, Ordering::SeqCst);
            })
        };

        // The worker must finish while we still hold the lock. A spawn that
        // needs the lock blocks here, so the wait doubles as the assertion.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !finished.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let completed_while_locked = finished.load(Ordering::SeqCst);

        drop(held);
        worker.join().expect("worker must not deadlock");

        assert!(
            completed_while_locked,
            "spawn_all did not finish while the supervisor lock was held: it \
             must create processes without taking `inner`"
        );
    }
}

#[cfg(test)]
mod resources_tests {
    use super::*;

    fn res(pairs: &[(&str, f64)]) -> Resources {
        let mut r = Resources::default();
        for (n, t) in pairs {
            r.insert_total(n, *t);
        }
        r
    }

    /// The pre-change algorithm, kept as the oracle.
    fn pattern_for_the_old_way(r: &Resources, name: &str) -> Option<String> {
        if r.totals.contains_key(name) {
            return None;
        }
        let mut patterns: Vec<&String> = r.totals.keys().filter(|k| k.contains('*')).collect();
        patterns.sort();
        patterns
            .into_iter()
            .find(|p| glob_matches(p, name))
            .map(|p| p.to_string())
    }

    #[test]
    fn pattern_lookup_matches_the_previous_implementation() {
        let r = res(&[
            ("gpu", 2.0),
            ("db", 1.0),
            ("tag:*", 4.0),
            ("flow:*/nightly", 3.0),
            ("flow:*", 8.0),
        ]);
        let names = [
            "gpu",
            "db",
            "tag:urgent",
            "flow:p/nightly",
            "flow:p/daily",
            "other",
        ];
        for n in names {
            assert_eq!(
                r.pattern_for(n).map(str::to_string),
                pattern_for_the_old_way(&r, n),
                "pattern_for differs for {n}"
            );
        }
    }

    #[test]
    fn totals_are_unchanged() {
        let r = res(&[("gpu", 2.0), ("tag:*", 4.0)]);
        assert_eq!(r.total("gpu"), 2.0, "explicit total");
        assert_eq!(r.total("tag:urgent"), 4.0, "pattern total");
        assert_eq!(r.total("nothing"), 1.0, "default is 1");
    }

    #[test]
    fn the_first_pattern_in_sorted_order_wins() {
        // Sorting is by the raw pattern, so a prefix sorts before the longer
        // pattern that extends it: "flow:*" < "flow:*/nightly". Both match
        // "flow:p/nightly", and the shorter one is the one that applies. This
        // is the pre-existing behaviour and it is user-visible through the
        // limit that takes effect, so it is pinned here.
        let r = res(&[("flow:*/nightly", 3.0), ("flow:*", 8.0)]);
        assert_eq!(r.pattern_for("flow:p/nightly"), Some("flow:*"));
        assert_eq!(r.total("flow:p/nightly"), 8.0);

        // A bare "*" sorts before any named pattern and shadows it.
        let r2 = res(&[("gpu*", 2.0), ("*", 9.0)]);
        assert_eq!(r2.pattern_for("gpu1"), Some("*"));
        assert_eq!(r2.total("gpu1"), 9.0);
    }

    #[test]
    fn an_explicit_total_beats_a_matching_pattern() {
        let r = res(&[("tag:*", 4.0), ("tag:urgent", 1.0)]);
        assert_eq!(r.pattern_for("tag:urgent"), None);
        assert_eq!(r.total("tag:urgent"), 1.0);
    }

    #[test]
    fn a_pattern_registered_later_is_found() {
        let mut r = res(&[]);
        assert_eq!(r.total("tag:x"), 1.0, "no patterns yet");
        r.insert_total("tag:*", 6.0);
        assert_eq!(r.total("tag:x"), 6.0, "the new pattern applies");
        // And a non-pattern registered later is not mistaken for one.
        r.insert_total("plain", 2.0);
        assert_eq!(r.pattern_for("plain"), None);
    }

    #[test]
    fn updating_a_pattern_value_does_not_disturb_it() {
        let mut r = res(&[("tag:*", 4.0)]);
        r.insert_total("tag:*", 9.0);
        assert_eq!(r.total("tag:x"), 9.0);
        // Registered once, so the list must not have grown a duplicate.
        assert_eq!(r.patterns.iter().filter(|p| *p == "tag:*").count(), 1);
    }

    #[test]
    fn is_declared_tracks_both_kinds() {
        let r = res(&[("gpu", 1.0), ("tag:*", 2.0)]);
        assert!(r.is_declared("gpu"));
        assert!(r.is_declared("tag:x"));
        assert!(!r.is_declared("other"));
    }

    #[test]
    fn eviction_keeps_held_and_declared_instances() {
        let mut r = res(&[("gpu", 2.0)]);
        // Fill past the retention threshold with idle, undeclared instances.
        for i in 0..(IDLE_INSTANCES_KEPT + 50) {
            r.used.insert(format!("tag:{i}"), 0.0);
        }
        r.used.insert("held".into(), 1.0); // in use, must survive
        r.used.insert("gpu".into(), 0.0); // declared, must survive
        assert!(r.used.len() > IDLE_INSTANCES_KEPT);

        r.evict_idle();

        assert!(
            r.used.contains_key("held"),
            "an in-use instance was evicted"
        );
        assert!(
            r.used.contains_key("gpu"),
            "a declared instance was evicted"
        );
        assert!(
            !r.used.contains_key("tag:0"),
            "an idle undeclared instance survived"
        );
        assert!(r.used.len() <= IDLE_INSTANCES_KEPT, "not pruned enough");
    }

    #[test]
    fn a_small_table_is_never_pruned() {
        let mut r = res(&[("gpu", 1.0)]);
        for i in 0..10 {
            r.used.insert(format!("tag:{i}"), 0.0);
        }
        r.evict_idle();
        assert_eq!(r.used.len(), 10, "a table below the threshold was pruned");
    }

    #[test]
    fn releasing_a_lease_returns_the_amount_to_zero() {
        let mut r = res(&[]);
        let lease = r.acquire(1, &[("tag:a".into(), 1.0), ("tag:b".into(), 2.0)]);
        assert_eq!(r.used.get("tag:a").copied(), Some(1.0));
        r.release_lease(1, lease);
        // Zero-usage instances are retained up to the retention threshold; the
        // amount is what matters, and `free` treats a zero entry as absent.
        assert_eq!(r.used.get("tag:a").copied(), Some(0.0));
        assert_eq!(r.free("tag:a"), 1.0, "not fully released");
        assert_eq!(r.free("tag:b"), 1.0, "default total, fully released");

        // Past the threshold they are pruned.
        for i in 0..(IDLE_INSTANCES_KEPT + 10) {
            r.used.insert(format!("other:{i}"), 0.0);
        }
        r.evict_idle();
        assert!(!r.used.contains_key("tag:a"), "idle instance not pruned");
    }

    #[test]
    fn can_acquire_reports_the_blocking_name() {
        let mut r = res(&[("tag:*", 1.0)]);
        // A pattern total is the limit for each name it covers, not a shared
        // pool, so exhausting one name does not block another.
        r.acquire(1, &[("tag:a".into(), 1.0)]);
        assert_eq!(r.can_acquire(&[("tag:b".into(), 1.0)]), None);
        // The name that is exhausted is the one reported.
        assert_eq!(
            r.can_acquire(&[("tag:a".into(), 1.0)]),
            Some("tag:a".to_string())
        );
        // With the limit raised, it is acquirable again.
        r.insert_total("tag:*", 2.0);
        assert_eq!(r.can_acquire(&[("tag:a".into(), 1.0)]), None);
    }

    #[test]
    fn can_acquire_reports_the_first_blocking_need() {
        let mut r = res(&[("tag:*", 1.0)]);
        r.acquire(1, &[("tag:b".into(), 1.0)]);
        let needs = vec![
            ("tag:a".to_string(), 1.0), // fine
            ("tag:b".to_string(), 1.0), // blocked
            ("tag:c".to_string(), 1.0),
        ];
        assert_eq!(r.can_acquire(&needs), Some("tag:b".to_string()));
    }
}

/// Engine-availability lookups: the set is built once per queue scan so the
/// per-run test is a hash lookup rather than a scan of the engine table.
#[cfg(test)]
mod idle_key_tests {
    use super::*;

    fn k(module: &str) -> EngineKey {
        EngineKey {
            source_dir: "/nonexistent".into(),
            module: module.into(),
            isolated: false,
            nice: 0,
        }
    }

    /// An engine row in the given state.
    fn engine(id: &str, key: &EngineKey, busy: bool, exiting: bool) -> Engine {
        let mut e = Engine::new(id.to_string(), key.clone(), None, None, false);
        e.current_run = busy.then_some(1);
        e.exit_requested = exiting;
        e
    }

    fn inner_with(engines: Vec<Engine>) -> Inner {
        let mut inner = Inner::default();
        for e in engines {
            inner.engines.insert(e.id.clone(), e);
        }
        inner
    }

    fn slot(name: &str, flows: &[i64]) -> WorkerSlot {
        WorkerSlot {
            name: name.into(),
            processors: 2,
            state: "online".into(),
            eligible: flows.iter().copied().collect(),
            last_seen: Instant::now(),
            commands: Vec::new(),
            broken: Default::default(),
            next_engine: 0,
        }
    }

    #[test]
    fn an_idle_engine_puts_its_key_in_the_set() {
        let key = k("etl");
        let inner = inner_with(vec![engine("e1", &key, false, false)]);
        assert!(inner.idle_engine_keys().contains(&key));
    }

    #[test]
    fn a_busy_engine_does_not() {
        let key = k("etl");
        let inner = inner_with(vec![engine("e1", &key, true, false)]);
        assert!(
            inner.idle_engine_keys().is_empty(),
            "a running engine counts as idle"
        );
    }

    #[test]
    fn an_exiting_engine_does_not() {
        let key = k("etl");
        let inner = inner_with(vec![engine("e1", &key, false, true)]);
        assert!(
            inner.idle_engine_keys().is_empty(),
            "an engine on its way out counts as idle"
        );
    }

    #[test]
    fn only_idle_keys_appear() {
        let idle = k("etl");
        let busy = k("ml");
        let inner = inner_with(vec![
            engine("e1", &idle, false, false),
            engine("e2", &busy, true, false),
        ]);
        let set = inner.idle_engine_keys();
        assert!(set.contains(&idle));
        assert!(!set.contains(&busy));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn has_idle_other_ignores_the_asking_engine() {
        let key = k("etl");
        // Only this engine is idle, so it must not count itself.
        let alone = inner_with(vec![engine("me", &key, false, false)]);
        let set = alone.idle_engine_keys();
        assert!(
            !alone.has_idle_other(&set, &key, "me"),
            "the engine asking for work counted itself"
        );

        // A second idle engine for the same key does count.
        let two = inner_with(vec![
            engine("me", &key, false, false),
            engine("other", &key, false, false),
        ]);
        let set2 = two.idle_engine_keys();
        assert!(two.has_idle_other(&set2, &key, "me"));
    }

    #[test]
    fn has_idle_other_is_false_when_the_key_has_none() {
        let a = k("etl");
        let b = k("ml");
        let inner = inner_with(vec![engine("e1", &a, false, false)]);
        let set = inner.idle_engine_keys();
        assert!(!inner.has_idle_other(&set, &b, "e1"));
    }

    #[test]
    fn worker_eligible_flows_unions_every_worker() {
        let mut inner = Inner::default();
        inner.workers.insert(1, slot("w1", &[1, 2]));
        inner.workers.insert(2, slot("w2", &[2, 3]));

        let flows = inner.worker_eligible_flows();
        assert_eq!(flows.len(), 3, "a flow claimed twice appears once");
        assert!(flows.contains(&1) && flows.contains(&2) && flows.contains(&3));
        assert!(!flows.contains(&4));
    }

    #[test]
    fn no_workers_means_no_eligible_flows() {
        assert!(Inner::default().worker_eligible_flows().is_empty());
    }

    // ---- end to end through take_waiting_marks ---------------------------

    fn sup_with_cap(max: usize) -> Supervisor {
        let config: ServeConfig = serde_json::from_value(json!({
            "home": std::env::temp_dir(),
            "max_engines": max,
        }))
        .unwrap();
        Supervisor::new(&config)
    }

    /// A run in `Running`, for adopting a busy engine.
    fn busy_run(id: i64) -> Run {
        Run {
            id,
            external_id: cereyan_core::new_id(),
            flow_id: 1,
            flow_name: "f".into(),
            project: "p".into(),
            group: String::new(),
            name: format!("r{id}"),
            parameters: serde_json::Map::new(),
            tags: vec![],
            attributes: serde_json::Map::new(),
            state: cereyan_core::State::new(cereyan_core::StateType::Running),
            failure_count: 0,
            crash_count: 0,
            created_at: 0,
            start_time: Some(0),
            end_time: None,
            total_run_time: None,
            engine_pid: Some(4242),
            engine_id: Some("adopted".into()),
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

    fn run_in(run_id: i64, module: &str) -> QueuedRun {
        QueuedRun {
            run_id,
            key: k(module),
            priority: 0,
            order: run_id,
            needs: Vec::new(),
            not_before: None,
            flow_id: 1,
            remote_ok: true,
            prefer_worker: None,
            prefer_until: 0,
        }
    }

    /// A full pool whose only engine is busy: every queued run lacks a slot.
    ///
    /// `max_engines` is clamped to at least 1, so the pool is filled by
    /// adopting a running engine rather than by setting the cap to zero.
    #[test]
    fn a_full_pool_with_only_a_busy_engine_marks_every_run() {
        let s = sup_with_cap(1);
        s.adopt(&busy_run(99), k("other"));
        assert_eq!(s.max_engines(), 1, "cap should be one engine");
        for i in 1..=3 {
            s.enqueue(run_in(i, "etl"));
        }
        let marks = s.take_waiting_marks();
        let mut ids: Vec<i64> = marks.iter().map(|(id, _)| *id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3], "not every run was marked");
        assert!(
            marks.iter().all(|(_, r)| r == "no engine slot"),
            "wrong reasons: {marks:?}"
        );
    }

    /// The same pool, but the queued run's key matches the idle-able engine, so
    /// the key lookup must find it and not report a slot shortage.
    #[test]
    fn a_matching_idle_engine_clears_the_slot_shortage() {
        let s = sup_with_cap(2);
        // An engine that exists but is idle: adopted then released.
        s.adopt(&busy_run(99), k("etl"));
        {
            let mut inner = s.inner.lock().unwrap();
            for e in inner.engines.values_mut() {
                e.current_run = None;
            }
        }
        s.enqueue(run_in(1, "etl"));
        assert!(
            s.take_waiting_marks().is_empty(),
            "an idle engine for the run's key was not seen"
        );
    }

    /// Note on `take_waiting_marks`: the slot check is gated on `engines_full`,
    /// which requires *every* engine to be busy, so the idle-key set is always
    /// empty when that branch runs. The lookup is equivalent there by
    /// construction — a cost reduction, not a behaviour change.
    ///
    /// `queue_snapshot` is where the key decides the answer, and the next two
    /// tests cover that.
    #[test]
    fn the_queue_page_reports_an_idle_engine_for_a_matching_key() {
        let s = sup_with_cap(1);
        s.adopt(&busy_run(99), k("etl"));
        {
            // One engine, idle: the pool is full, so `room` is false and the
            // answer turns on whether the run's key matches.
            let mut inner = s.inner.lock().unwrap();
            for e in inner.engines.values_mut() {
                e.current_run = None;
            }
        }
        s.enqueue(run_in(1, "etl")); // key matches the idle engine
        s.enqueue(run_in(2, "ml")); // key does not

        let (_, line, _) = s.queue_snapshot(10);
        let entry = |id: i64| {
            line.iter()
                .find(|l| l.run_id == id)
                .map(|l| (l.can_start, l.reason.clone()))
        };
        assert_eq!(
            entry(1),
            Some((true, None)),
            "a run matching the idle engine's key was reported as blocked"
        );
        assert_eq!(
            entry(2),
            Some((false, Some("no processor".to_string()))),
            "a run with no engine for its key was reported as startable"
        );
    }

    /// With no idle engine, every key is equally short of one.
    #[test]
    fn the_queue_page_reports_a_shortage_for_every_key() {
        let s = sup_with_cap(1);
        s.adopt(&busy_run(99), k("etl")); // stays busy
        s.enqueue(run_in(1, "etl"));
        s.enqueue(run_in(2, "ml"));

        let (_, line, _) = s.queue_snapshot(10);
        assert_eq!(line.len(), 2);
        for l in &line {
            assert!(!l.can_start, "run {} should not start", l.run_id);
            assert_eq!(l.reason.as_deref(), Some("no processor"));
        }
    }

    /// With room in the pool, nothing is reported as lacking a slot.
    #[test]
    fn room_in_the_pool_marks_nothing() {
        let s = sup_with_cap(4);
        for i in 1..=3 {
            s.enqueue(run_in(i, "etl"));
        }
        assert!(s.take_waiting_marks().is_empty());
    }

    /// A run blocked on a resource reports the resource, not the engine slot.
    #[test]
    fn a_resource_block_reports_the_resource_name() {
        let s = sup_with_cap(0);
        s.set_total("gpu", 0.0);
        let mut q = run_in(1, "etl");
        q.needs = vec![("gpu".into(), 1.0)];
        s.enqueue(q);
        assert_eq!(s.take_waiting_marks(), vec![(1, "gpu".to_string())]);
    }

    /// A run that is not yet due is skipped entirely.
    #[test]
    fn a_run_not_yet_due_is_not_marked() {
        let s = sup_with_cap(0);
        let mut q = run_in(1, "etl");
        q.not_before = Some(cereyan_core::now_micros() + 10_000_000);
        s.enqueue(q);
        assert!(s.take_waiting_marks().is_empty());
    }
}

#[cfg(test)]
mod busy_count_tests {
    use super::*;

    fn k(module: &str) -> EngineKey {
        EngineKey {
            source_dir: "/nonexistent".into(),
            module: module.into(),
            isolated: false,
            nice: 0,
        }
    }

    fn sup_with_engines(specs: &[(&str, bool, bool)]) -> Supervisor {
        // (module, busy, exit_requested)
        let config: ServeConfig = serde_json::from_value(serde_json::json!({
            "home": std::env::temp_dir(),
            "max_engines": 64,
        }))
        .unwrap();
        let s = Supervisor::new(&config);
        for (i, (module, busy, exiting)) in specs.iter().enumerate() {
            let key = k(module);
            let mut e = Engine::new(format!("e{i}"), key, None, None, false);
            e.current_run = busy.then_some(100 + i as i64);
            e.exit_requested = *exiting;
            s.inner.lock().unwrap().engines.insert(format!("e{i}"), e);
        }
        s
    }

    /// The invariant: the sampler previously derived this number from
    /// `engines_snapshot`, so the two must agree.
    #[test]
    fn the_count_agrees_with_the_snapshot() {
        for specs in [
            vec![("etl", false, false)],
            vec![("etl", true, false)],
            vec![
                ("etl", true, false),
                ("ml", false, false),
                ("reports", true, false),
            ],
            vec![("etl", false, true), ("ml", true, true)],
            vec![],
        ] {
            let s = sup_with_engines(&specs);
            let from_snapshot = s
                .engines_snapshot()
                .iter()
                .filter(|e| !e["current_run"].is_null())
                .count();
            assert_eq!(
                s.engines_busy_count(),
                from_snapshot,
                "count disagrees with the snapshot for {specs:?}"
            );
        }
    }

    #[test]
    fn an_idle_engine_is_not_counted() {
        let s = sup_with_engines(&[("etl", false, false)]);
        assert_eq!(s.engines_busy_count(), 0);
    }

    #[test]
    fn every_busy_engine_is_counted() {
        let s = sup_with_engines(&[
            ("etl", true, false),
            ("ml", true, false),
            ("reports", false, false),
            ("mail", true, false),
        ]);
        assert_eq!(s.engines_busy_count(), 3);
    }

    /// An engine told to exit while holding a run is still running something, so
    /// it counts. This is the pre-existing behaviour and the test pins it.
    #[test]
    fn an_exiting_engine_holding_a_run_is_counted() {
        let s = sup_with_engines(&[("etl", true, true)]);
        assert_eq!(
            s.engines_busy_count(),
            1,
            "an exiting engine that still holds a run is busy"
        );
    }

    #[test]
    fn no_engines_counts_zero() {
        let config: ServeConfig = serde_json::from_value(serde_json::json!({
            "home": std::env::temp_dir(), "max_engines": 4,
        }))
        .unwrap();
        assert_eq!(Supervisor::new(&config).engines_busy_count(), 0);
    }

    /// This count is not the pool's occupancy: an idle engine occupies the pool
    /// without being busy. If the two were ever conflated, this would catch it.
    #[test]
    fn the_count_is_not_pool_occupancy() {
        let s = sup_with_engines(&[("etl", false, false), ("ml", false, false)]);
        assert_eq!(s.engines_busy_count(), 0, "nothing is running");
        let inner = s.inner.lock().unwrap();
        assert_eq!(
            inner.occupying(),
            2,
            "both idle engines still occupy the pool, which is a different number"
        );
    }

    /// The snapshot must keep serving its JSON consumers unchanged.
    #[test]
    fn the_snapshot_still_reports_every_engine() {
        let s = sup_with_engines(&[("etl", true, false), ("ml", false, false)]);
        let snap = s.engines_snapshot();
        assert_eq!(snap.len(), 2);
        for e in &snap {
            for k in [
                "id",
                "pid",
                "module",
                "source_dir",
                "isolated",
                "nice",
                "runs_done",
                "current_run",
                "adopted",
                "exit_requested",
                "uptime_secs",
            ] {
                assert!(e.get(k).is_some(), "the snapshot lost the key {k}");
            }
        }
        let busy: Vec<_> = snap
            .iter()
            .filter(|e| !e["current_run"].is_null())
            .collect();
        assert_eq!(busy.len(), 1);
        assert_eq!(busy[0]["module"], "etl");
    }
}

#[cfg(test)]
mod resource_row_tests {
    use super::*;

    fn sup(max: usize) -> Supervisor {
        let config: ServeConfig = serde_json::from_value(json!({
            "home": std::env::temp_dir(),
            "max_engines": max,
        }))
        .unwrap();
        Supervisor::new(&config)
    }

    /// A supervisor with declared totals including a pattern, plus one resource
    /// in use that was never declared.
    fn sup_with_resources() -> Supervisor {
        let config: ServeConfig = serde_json::from_value(json!({
            "home": std::env::temp_dir(),
            "max_engines": 4,
            "resources": {
                "cpu": 4,
                "memory": 1024,
                "gpu-*": 2,
                "exact": 7,
            },
        }))
        .unwrap();
        let s = Supervisor::new(&config);
        {
            let mut r = s.resources.lock().unwrap();
            // In use, never declared: `total` falls back to the default.
            r.used.insert("undeclared".into(), 2.0);
            r.used.insert("gpu-0".into(), 1.0);
            r.used.insert("exact".into(), 0.0);
        }
        s
    }

    /// The rows and the serialised snapshot are two representations of one thing.
    /// The snapshot is built from the rows, so this checks the *shape* survived
    /// that derivation -- in particular that `name` is the object key and not a
    /// field inside the entry, which is what the settings endpoint and the two
    /// MCP resource tools receive.
    #[test]
    fn the_rows_and_the_snapshot_agree() {
        let s = sup_with_resources();
        let rows = s.resource_rows();
        let obj = s.resources_snapshot();
        let obj = obj.as_object().expect("the snapshot is an object");

        assert_eq!(
            rows.len(),
            obj.len(),
            "row count differs from snapshot size"
        );
        for r in &rows {
            let e = obj
                .get(&r.name)
                .unwrap_or_else(|| panic!("no snapshot entry for {}", r.name));
            let e = e.as_object().expect("an entry is an object");
            assert_eq!(
                e["total"],
                serde_json::json!(r.total),
                "total for {}",
                r.name
            );
            assert_eq!(e["used"], serde_json::json!(r.used), "used for {}", r.name);
            let want_pattern = r.pattern.clone().map(serde_json::Value::String);
            assert_eq!(
                e.get("pattern"),
                want_pattern.as_ref(),
                "pattern for {}",
                r.name
            );
            // The name is the key, never a field inside the entry.
            assert!(
                e.get("name").is_none(),
                "{} must not carry a name field; it is the object key",
                r.name
            );
        }
        // And the entry carries exactly the keys it always did.
        for (name, e) in obj {
            let e = e.as_object().unwrap();
            let mut keys: Vec<&str> = e.keys().map(|k| k.as_str()).collect();
            keys.sort();
            assert_eq!(
                keys,
                if e.contains_key("pattern") {
                    vec!["pattern", "total", "used"]
                } else {
                    vec!["total", "used"]
                },
                "the keys of {name} changed"
            );
        }
    }

    #[test]
    fn a_row_carries_its_name_total_usage_and_pattern() {
        let s = sup_with_resources();
        let rows = s.resource_rows();
        let by = |n: &str| rows.iter().find(|r| r.name == n).cloned();

        // An exact declaration: its own total, and no pattern.
        let exact = by("exact").expect("exact");
        assert_eq!(exact.total, 7.0);
        assert_eq!(exact.used, 0.0, "declared but unused is zero usage");
        assert_eq!(exact.pattern, None, "an exact total needs no pattern");

        // A pattern match: the pattern's total, and the pattern itself.
        let gpu = by("gpu-0").expect("gpu-0");
        assert_eq!(gpu.total, 2.0, "the pattern's total applies");
        assert_eq!(gpu.used, 1.0);
        assert_eq!(gpu.pattern.as_deref(), Some("gpu-*"));

        // Never declared, but in use: listed, with the default total.
        let undeclared = by("undeclared").expect("undeclared");
        assert_eq!(undeclared.used, 2.0);
        assert_eq!(
            undeclared.total, 1.0,
            "an undeclared resource's default total"
        );
        assert_eq!(undeclared.pattern, None);

        // Sorted by name, as the snapshot has always been.
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "rows are sorted by name");
    }

    /// The case the old `unwrap_or(0.0)` hid: a resource whose usage really is
    /// zero, and one whose usage is not. Both must be reported as themselves.
    #[test]
    fn zero_usage_is_the_resource_s_own_zero() {
        let s = sup_with_resources();
        let rows = s.resource_rows();
        let zero = rows.iter().find(|r| r.name == "exact").unwrap();
        let nonzero = rows.iter().find(|r| r.name == "gpu-0").unwrap();
        assert_eq!(zero.used, 0.0);
        assert_eq!(nonzero.used, 1.0);
        // The snapshot reports them the same way, with no fallback in between.
        let obj = s.resources_snapshot();
        assert_eq!(obj["exact"]["used"], serde_json::json!(0.0));
        assert_eq!(obj["gpu-0"]["used"], serde_json::json!(1.0));
    }

    /// The snapshot must be byte-identical to what the hand-built `json!`
    /// produced, because three endpoints return it as their response body. This
    /// re-implements the old code and compares, over a set that includes a
    /// pattern match and an undeclared resource.
    #[test]
    fn the_snapshot_is_identical_to_the_previous_implementation() {
        fn the_old_way(s: &Supervisor) -> serde_json::Value {
            let r = s.resources.lock().unwrap_or_else(|e| e.into_inner());
            let mut names: Vec<&String> = r.totals.keys().chain(r.used.keys()).collect();
            names.sort();
            names.dedup();
            serde_json::Value::Object(
                names
                    .into_iter()
                    .map(|n| {
                        let mut entry = json!({
                            "total": r.total(n),
                            "used": r.used.get(n).copied().unwrap_or(0.0),
                        });
                        if let Some(p) = r.pattern_for(n) {
                            entry["pattern"] = serde_json::Value::String(p.to_string());
                        }
                        (n.clone(), entry)
                    })
                    .collect(),
            )
        }

        let s = sup_with_resources();
        let old = the_old_way(&s);
        let new = s.resources_snapshot();
        assert_eq!(
            new, old,
            "the snapshot's shape changed for its three JSON callers"
        );
        // String comparison too, so key ordering is covered and not just values.
        assert_eq!(
            serde_json::to_string(&new).unwrap(),
            serde_json::to_string(&old).unwrap()
        );
    }

    #[test]
    fn no_resources_yields_no_rows_and_an_empty_object() {
        let s = sup(4);
        // Only whatever `Supervisor::new` declares by default.
        let rows = s.resource_rows();
        let obj = s.resources_snapshot();
        assert_eq!(
            rows.len(),
            obj.as_object().unwrap().len(),
            "rows and snapshot disagree when empty"
        );
    }

    #[test]
    fn declaring_a_resource_total_updates_the_row_and_the_snapshot() {
        let s = sup(4);
        let before = s.resource_rows();
        {
            let mut r = s.resources.lock().unwrap();
            r.insert_total("brand-new", 12.0);
            r.used.insert("brand-new".into(), 3.0);
        }
        let after = s.resource_rows();
        assert_eq!(after.len(), before.len() + 1, "the new resource is listed");
        let row = after.iter().find(|r| r.name == "brand-new").unwrap();
        assert_eq!(row.total, 12.0);
        assert_eq!(row.used, 3.0);
        let obj = s.resources_snapshot();
        assert_eq!(obj["brand-new"]["total"], serde_json::json!(12.0));
        assert_eq!(obj["brand-new"]["used"], serde_json::json!(3.0));
    }
}
