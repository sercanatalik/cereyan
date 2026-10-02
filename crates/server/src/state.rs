//! Shared server state and the transition path used by every writer.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use cereyan_core::{now_micros, Event, Run, State, StateType, TaskRun};
use cereyan_store::{Store, StoreError};
use serde_json::json;
use tokio::sync::watch;

use crate::custom::RouteDispatcher;
use crate::index::ActiveIndex;
use crate::scheduler::Scheduler;
use crate::stream::Broadcaster;
use crate::supervisor::{EngineKey, Supervisor};
use crate::timer::Timer;
use crate::{ServeConfig, ServerError};

pub struct AppState {
    pub config: ServeConfig,
    pub store: Arc<Store>,
    pub index: ActiveIndex,
    pub stream: Broadcaster,
    pub supervisor: Supervisor,
    pub scheduler: Scheduler,
    pub timer: Timer,
    pub dispatcher: Option<Arc<dyn RouteDispatcher>>,
    /// Validates credentials other than the static token; set only when auth is enabled.
    pub authenticator: Option<Arc<dyn crate::auth::Authenticator>>,
    pub live_flows: RwLock<HashSet<i64>>,
    pub addr: SocketAddr,
    pub started_at: i64,
    pub shutting_down: AtomicBool,
    pub shutdown: watch::Receiver<bool>,
    weak: RwLock<Option<std::sync::Weak<AppState>>>,
    pub rules: crate::rules::RulesState,
    pub rule_dispatcher: RwLock<Option<Arc<dyn crate::rules::RuleDispatcher>>>,
    /// Mutable settings (retention days), persisted to cereyan.toml on change.
    pub mcp: crate::mcp::Sessions,
    /// The dashboard's last hour of queue depth, sampled every five seconds.
    pub samples: crate::metrics::Samples,
    pub retain_days: std::sync::atomic::AtomicI64,
    pub retain_runs_days: std::sync::atomic::AtomicI64,
    pub retain_failed_runs_days: std::sync::atomic::AtomicI64,
    pub keep_last_runs_per_flow: std::sync::atomic::AtomicI64,
    pub backup_every: std::sync::atomic::AtomicI64,
    pub backup_keep: std::sync::atomic::AtomicI64,
    pub retain_checkpoints_days: std::sync::atomic::AtomicI64,
    pub crash_retries_default: std::sync::atomic::AtomicI64,
    /// UI title in effect: `[ui] title` from cereyan.toml, or `cereyan`.
    pub title: RwLock<String>,
    /// Where each setting came from, keyed `table.key`; Settings edits mark theirs.
    pub sources: RwLock<std::collections::HashMap<String, crate::SettingSource>>,
    /// Set while `POST /api/database/reset` runs; run creation answers 503.
    pub resetting: AtomicBool,
    /// The global pause while the scheduler is paused (`scheduler.paused` in kv).
    pub pause: RwLock<Option<crate::scheduler::Pause>>,
    /// Fingerprints of the server's own modules, recomputed when a file changes.
    pub fingerprints: crate::fingerprint::Fingerprints,
    /// Backfills whose completion event has been emitted. Reset on server start.
    pub backfill_emitted: std::sync::Mutex<std::collections::HashSet<i64>>,
    /// Cached dependency graph: maps upstream flow IDs to the flows depending on
    /// them. Invalidated on flow create/update/delete and on project deletion,
    /// rebuilt lazily and exactly once on next access.
    pub dep_graph: DepGraphCache,
    /// Runs whose overdue event has been reported. Reset on server start.
    pub overdue_reported: std::sync::Mutex<std::collections::HashSet<i64>>,
}

/// The dependency graph: upstream flow id → the flows that depend on it.
///
/// Shared behind an `Arc` because it is read on every run admission and is
/// immutable once built. The inner `Arc<[i64]>` means a caller wanting one
/// upstream's dependents clones a slice handle, not a `Vec`.
/// The flows that declare a dependency on one upstream, and the highest priority
/// any of them declares.
///
/// The ids are behind an `Arc` because they are read far more often than they
/// change, and the priority is precomputed because `effective_priority` only ever
/// wants the maximum. Both come from the same `FlowOptions`, parsed once while the
/// graph is built — which is why the read path needs no store read for either.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Dependents {
    pub ids: Box<[i64]>,
    pub max_priority: i64,
}

/// While the graph is being built: a mutable accumulator that becomes a
/// [`Dependents`]. Split so the public type can hold a boxed slice, which is
/// cheaper to share and cannot be appended to by a reader.
#[derive(Default)]
struct DependentsBeingBuilt {
    ids: Vec<i64>,
    max_priority: i64,
}

impl DependentsBeingBuilt {
    /// Record a dependent, keeping the highest priority seen.
    fn push(&mut self, id: i64, priority: i64) {
        self.ids.push(id);
        self.max_priority = self.max_priority.max(priority);
    }

    fn finish(self) -> Dependents {
        Dependents {
            ids: self.ids.into_boxed_slice(),
            max_priority: self.max_priority,
        }
    }
}

/// Upstream flow id → the flows that declared `after=` it.
pub type DepGraph = std::sync::Arc<std::collections::HashMap<i64, std::sync::Arc<Dependents>>>;

/// Holds the dependency graph and rebuilds it at most once after an
/// invalidation, however many threads ask at the same time.
#[derive(Default)]
pub struct DepGraphCache {
    slot: RwLock<Option<DepGraph>>,
    /// Held while a rebuild runs, so a burst of lookups after an invalidation
    /// costs one build rather than one per caller. Separate from `slot` so
    /// readers are never blocked by an in-flight build.
    building: std::sync::Mutex<()>,
    /// Bumped by every invalidation. A build publishes its graph only if no
    /// invalidation landed while it read the flows; otherwise the graph it
    /// built predates the change and caching it would hide that change until
    /// the next one.
    generation: std::sync::atomic::AtomicU64,
}

impl DepGraphCache {
    /// The graph, calling `build` only if it is missing or was invalidated.
    ///
    /// Rebuilds are serialised by `building`, then re-checked: the first thread
    /// through builds and publishes, and the rest find the warm cache and return
    /// without building. Without the re-check a burst after an invalidation
    /// would run one store read per caller.
    pub fn get_or_build(&self, build: impl FnOnce() -> DepGraph) -> DepGraph {
        self.try_get_or_build(|| Some(build()))
    }

    /// As `get_or_build`, for a build that can fail: a failed build returns an
    /// empty graph to this caller and caches nothing, so the next lookup tries
    /// again instead of every lookup seeing no dependents until a flow changes.
    pub fn try_get_or_build(&self, build: impl FnOnce() -> Option<DepGraph>) -> DepGraph {
        use std::sync::atomic::Ordering;
        if let Some(cached) = self.slot.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            return cached.clone();
        }
        let _guard = self.building.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = self.slot.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            return cached.clone();
        }
        let generation = self.generation.load(Ordering::SeqCst);
        let Some(built) = build() else {
            return DepGraph::default();
        };
        let mut slot = self.slot.write().unwrap_or_else(|e| e.into_inner());
        if self.generation.load(Ordering::SeqCst) == generation {
            *slot = Some(built.clone());
        }
        built
    }

    /// Forget the graph; the next lookup rebuilds it.
    pub fn invalidate(&self) {
        let mut slot = self.slot.write().unwrap_or_else(|e| e.into_inner());
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *slot = None;
    }

    /// Is a graph currently held?
    pub fn is_populated(&self) -> bool {
        self.slot
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
}

/// Build the dependency graph from the flows: upstream flow id → dependents.
///
/// Every upstream a flow declares counts, not just the first, so this agrees
/// with `dispatch::trigger_dependents_at`. An upstream that does not exist has
/// no id, so the edge is skipped.
pub fn dep_graph_from_flows(
    flows: &[cereyan_core::Flow],
) -> std::collections::HashMap<i64, Dependents> {
    let mut graph: std::collections::HashMap<i64, DependentsBeingBuilt> =
        std::collections::HashMap::new();
    // (project, name) → id, from the same slice, so resolving an upstream is a
    // map lookup rather than a query.
    let by_key: std::collections::HashMap<(&str, &str), i64> = flows
        .iter()
        .map(|f| ((f.project.as_str(), f.name.as_str()), f.id))
        .collect();
    for flow in flows {
        let opts = cereyan_core::FlowOptions::from_map(&flow.options);
        let Some(after) = opts.after else { continue };
        // `opts.priority` is already parsed here, on the same object that yielded
        // `after`. Recording it costs nothing and saves every admission a store
        // read per dependent — `FlowOptions.priority` defaults to 0, and a
        // `max` against 0 cannot raise a priority above zero, so an undeclared
        // priority behaves exactly as the old read path gave it.
        let priority = opts.priority;
        let mut seen: Vec<String> = Vec::new();
        for upstream in after.upstreams() {
            // A hand-written spec can repeat a name; list the dependent once.
            if seen.contains(&upstream) {
                continue;
            }
            seen.push(upstream.clone());
            if let Some(id) = by_key.get(&(flow.project.as_str(), upstream.as_str())) {
                graph.entry(*id).or_default().push(flow.id, priority);
            }
        }
    }
    graph.into_iter().map(|(id, d)| (id, d.finish())).collect()
}

/// Outcome of a transition request: the run after the change, or the current
/// state when the rules rejected it.
pub enum TransitionResult {
    Accepted(Box<Run>),
    Rejected {
        reason: &'static str,
        current: Option<State>,
    },
}

/// kv key holding the answer a paused run resumes with.
pub fn run_input_key(run_id: i64) -> String {
    format!("run.input:{run_id}")
}

impl AppState {
    pub fn new(
        config: ServeConfig,
        store: Arc<Store>,
        dispatcher: Option<Arc<dyn RouteDispatcher>>,
        authenticator: Option<Arc<dyn crate::auth::Authenticator>>,
        addr: SocketAddr,
        shutdown: watch::Receiver<bool>,
    ) -> Result<AppState, ServerError> {
        let live: HashSet<i64> = config.live_flows.iter().copied().collect();
        let retain_days = config.retain_days;
        let retention = (
            config.retain_runs_days,
            config.retain_failed_runs_days,
            config.keep_last_runs_per_flow,
            config.backup_every,
            config.backup_keep,
            config.retain_checkpoints_days,
        );
        let crash_retries_default = config.crash_retries_default;
        let sources = config.sources.clone();
        let title = crate::ui::normalize_title(config.title.as_deref()).unwrap_or_else(|e| {
            eprintln!("warning: cereyan.toml: [ui] title ignored: {e}");
            crate::ui::DEFAULT_TITLE.into()
        });
        let state = AppState {
            supervisor: Supervisor::new(&config),
            mcp: crate::mcp::Sessions::default(),
            samples: crate::metrics::Samples::default(),
            scheduler: Scheduler::new(),
            timer: Timer::new(),
            config,
            store,
            index: ActiveIndex::new(),
            stream: Broadcaster::new(),
            dispatcher,
            authenticator,
            live_flows: RwLock::new(live),
            addr,
            started_at: now_micros(),
            shutting_down: AtomicBool::new(false),
            shutdown,
            weak: RwLock::new(None),
            rules: crate::rules::RulesState::default(),
            rule_dispatcher: RwLock::new(None),
            retain_days: std::sync::atomic::AtomicI64::new(retain_days),
            retain_runs_days: std::sync::atomic::AtomicI64::new(retention.0),
            retain_failed_runs_days: std::sync::atomic::AtomicI64::new(retention.1),
            keep_last_runs_per_flow: std::sync::atomic::AtomicI64::new(retention.2),
            backup_every: std::sync::atomic::AtomicI64::new(retention.3),
            backup_keep: std::sync::atomic::AtomicI64::new(retention.4),
            retain_checkpoints_days: std::sync::atomic::AtomicI64::new(retention.5),
            crash_retries_default: std::sync::atomic::AtomicI64::new(crash_retries_default),
            title: RwLock::new(title),
            sources: RwLock::new(sources),
            resetting: AtomicBool::new(false),
            pause: RwLock::new(None),
            fingerprints: crate::fingerprint::Fingerprints::default(),
            backfill_emitted: std::sync::Mutex::new(std::collections::HashSet::new()),
            dep_graph: DepGraphCache::default(),
            overdue_reported: std::sync::Mutex::new(std::collections::HashSet::new()),
        };
        Ok(state)
    }

    pub fn is_resetting(&self) -> bool {
        self.resetting.load(Ordering::SeqCst)
    }

    /// The UI title in effect.
    pub fn title(&self) -> String {
        self.title.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Record that the value of `key` (`table.key`) now comes from a Settings edit.
    pub fn mark_edited(&self, key: &str) {
        self.sources
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key.to_string(),
                crate::SettingSource {
                    source: "settings".into(),
                    name: None,
                },
            );
    }

    pub fn is_live(&self, flow_id: i64) -> bool {
        self.live_flows
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&flow_id)
    }

    pub fn discovery_path(&self) -> PathBuf {
        self.config.home.join("server.json")
    }

    /// Bound beyond loopback with no token required: the operator opted out.
    /// The global pause, if the scheduler is paused.
    pub fn pause(&self) -> Option<crate::scheduler::Pause> {
        self.pause.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn is_paused(&self) -> bool {
        self.pause
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Invalidate the dependency graph cache. Called on flow create/update/delete.
    pub fn invalidate_dep_graph(&self) {
        self.dep_graph.invalidate();
    }

    /// The dependency graph: upstream flow id → the flows that depend on it.
    ///
    /// Returned behind an `Arc` because this is read on every run admission, and
    /// the graph is immutable once built. An invalidated graph is rebuilt once
    /// even if several threads ask at the same time.
    pub fn dep_graph(&self) -> DepGraph {
        self.dep_graph.try_get_or_build(|| {
            let flows = self.store.list_flows(None).ok()?;
            Some(std::sync::Arc::new(
                dep_graph_from_flows(&flows)
                    .into_iter()
                    .map(|(k, v)| (k, std::sync::Arc::new(v)))
                    .collect(),
            ))
        })
    }

    pub fn exposed(&self) -> bool {
        !self.addr.ip().is_loopback() && self.config.token.is_none()
    }

    /// The file holding the token the server generated at start, if it did.
    pub fn token_file(&self) -> Option<String> {
        self.config
            .token_file
            .as_ref()
            .map(|p| p.display().to_string())
    }

    /// The URL every client uses: the dialable address followed by the base
    /// path, with no trailing slash. A listener on an unspecified address
    /// answers on loopback, and 0.0.0.0 is not somewhere a client can connect:
    /// Linux and macOS route it to loopback, Windows refuses it outright, so
    /// every discovery consumer there would fail to find the server.
    pub fn public_url(&self) -> String {
        let authority = if self.addr.ip().is_unspecified() {
            let loopback = if self.addr.is_ipv6() {
                "[::1]"
            } else {
                "127.0.0.1"
            };
            format!("{}:{}", loopback, self.addr.port())
        } else {
            self.addr.to_string()
        };
        format!("http://{}{}", authority, self.config.base_path)
    }

    pub fn write_discovery_file(&self) -> Result<(), ServerError> {
        // `host` records what was bound; `url` has to be dialable.
        let body = json!({
            "host": self.addr.ip().to_string(),
            "port": self.addr.port(),
            "base_path": self.config.base_path,
            "url": self.public_url(),
            "pid": std::process::id(),
            "started_at": self.started_at,
            "version": self.config.version,
            "auth": self.config.token.is_some(),
            "socket": self.config.socket.as_ref().map(|p| p.display().to_string()),
            "exposed": self.exposed(),
            "token_file": self.token_file(),
        });
        std::fs::write(
            self.discovery_path(),
            serde_json::to_vec_pretty(&body).unwrap(),
        )?;
        Ok(())
    }

    /// Rebuild the working set from the store and adopt or crash runs that
    /// were in flight when the previous server stopped.
    pub fn reconcile(&self) -> Result<(), ServerError> {
        let flows = self.store.list_flows(None)?;
        self.index.load_counts(
            self.store.run_counts()?,
            self.store.task_run_counts()?,
            flows
                .iter()
                .map(|f| (f.id, std::sync::Arc::<str>::from(f.project.as_str())))
                .collect(),
        );
        // A server that comes up inside a global pause must not dispatch runs
        // whose time passed while it was down; resuming re-arms them.
        let paused = self
            .store
            .kv_get(crate::scheduler::PAUSE_KEY)
            .ok()
            .flatten()
            .is_some();
        for run in self.store.active_runs()? {
            let Some(flow) = flows.iter().find(|f| f.id == run.flow_id) else {
                continue;
            };
            let key = EngineKey::from_flow(flow);
            match run.state.state_type {
                StateType::Scheduled | StateType::Pending if run.engine_pid.is_none() => {
                    // Never picked up: queue it again (future scheduled runs
                    // are re-armed by the scheduler instead).
                    self.index.adopt_run(&run, key.clone());
                    if !paused
                        && run
                            .scheduled_time
                            .map(|t| t <= now_micros())
                            .unwrap_or(true)
                    {
                        let remote_ok =
                            cereyan_core::FlowOptions::from_map(&flow.options).may_run_remotely();
                        self.supervisor
                            .enqueue_simple(run.id, flow.id, remote_ok, key);
                    }
                }
                StateType::Paused => {
                    // Waiting for a person, a time, an event, or a target: no
                    // engine holds it. Timed waits are re-armed by the scheduler.
                    self.index.adopt_run(&run, key);
                }
                _ => {
                    // A run on a worker has no process on this machine to check:
                    // adopt it and let its heartbeats decide.
                    let remote = run.engine_id.as_deref().is_some_and(|id| {
                        matches!(
                            crate::supervisor::Location::of_engine(id),
                            crate::supervisor::Location::Worker(_)
                        )
                    });
                    let alive = remote
                        || run
                            .engine_pid
                            .map(|p| crate::process::is_alive(p as u32))
                            .unwrap_or(false);
                    if alive {
                        self.index.adopt_run(&run, key.clone());
                        self.supervisor.adopt(&run, key);
                    } else {
                        self.index.adopt_run(&run, key);
                        let state = State::new(StateType::Crashed)
                            .with_message("server restarted while run was in progress");
                        let _ = self.transition_run(run.id, state, false);
                    }
                }
            }
        }
        self.supervisor.ensure_capacity(self);
        Ok(())
    }

    /// Run the shared rules through the store, then update the index and
    /// notify subscribers. Terminal states release the engine.
    pub fn transition_run(
        &self,
        run_id: i64,
        state: State,
        force: bool,
    ) -> Result<TransitionResult, StoreError> {
        let previous = self.store.get_run(run_id)?;
        let Some(previous) = previous else {
            return Err(StoreError::NotFound("run"));
        };
        let prev_state = if previous.state.timestamp == 0 {
            None
        } else {
            Some(previous.state.clone())
        };
        let new_state = match self.store.transition_run(run_id, state, force) {
            Ok(s) => s,
            Err(StoreError::RejectedWith { reason, current }) => {
                return Ok(TransitionResult::Rejected { reason, current });
            }
            Err(StoreError::Rejected(reason)) => {
                return Ok(TransitionResult::Rejected {
                    reason,
                    current: prev_state,
                });
            }
            Err(e) => return Err(e),
        };
        // The write returns the state as persisted, so the row is not read
        // back: `get_run` would run a per-row task-count aggregate for nothing.
        // The counters and timing the write updated are recomputed with the
        // same `RunCounters::apply` the store ran, so `end_time` and the
        // durations are not left at their pre-transition values.
        let mut run = previous.clone();
        let mut counters = cereyan_core::rules::RunCounters {
            failure_count: run.failure_count,
            crash_count: run.crash_count,
            start_time: run.start_time,
            end_time: run.end_time,
            total_run_time: run.total_run_time,
        };
        counters.apply(&new_state);
        run.failure_count = counters.failure_count;
        run.crash_count = counters.crash_count;
        run.start_time = counters.start_time;
        run.end_time = counters.end_time;
        run.total_run_time = counters.total_run_time;
        run.state = new_state;
        let was_active = self.index.get(run_id).is_some();
        if !was_active && !previous.state.is_terminal() {
            // Runs created before the index knew them (rare): insert first.
            if let Some(flow) = self.store.get_flow(run.flow_id)? {
                self.index.adopt_run(&previous, EngineKey::from_flow(&flow));
            }
        }
        self.index.transition(&run, prev_state.as_ref());
        if run.state.is_terminal() || run.state.state_type == StateType::Paused {
            // A paused run gives its engine and resources back while it waits.
            self.supervisor.run_finished(run_id);
            self.timer.remove_run_events(run_id);
            self.supervisor.ensure_capacity(self);
        }
        if run.state.state_type == StateType::Paused {
            // No engine owns a waiting run: a restart must not adopt the old
            // one, find it gone, and crash the run.
            let _ = self.store.set_run_engine(run_id, None, None);
            self.index.update(run_id, |r| {
                r.engine_pid = None;
                r.engine_id = None;
            });
        }
        if run.state.is_terminal() {
            // The answers belong to this run's questions and nothing asks them
            // again once it has ended.
            let _ = self.store.kv_delete(&run_input_key(run_id));
        }
        self.publish_run(&run);
        crate::events::emit_run_event(self, &run);
        if let Some(me) = self.self_ref() {
            crate::dispatch::after_transition(&me, &run, prev_state.as_ref());
        }
        Ok(TransitionResult::Accepted(Box::new(run)))
    }

    /// Weak self reference upgraded to an Arc, set once at startup.
    pub fn self_ref(&self) -> Option<Arc<AppState>> {
        self.weak
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|w| w.upgrade())
    }

    pub fn set_self(self: &Arc<Self>) {
        *self.weak.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(self));
    }

    /// Append an event to the store, publish it on the stream, and hand it to
    /// the rules engine. The resource defaults to the run or the flow.
    /// Record an event the engine owns. Taking [`EventName`] rather than a
    /// string is what keeps the catalogue in `cereyan-core` the only place an
    /// engine event is named; the `&str` form below is for custom events,
    /// whose names the engine does not own.
    pub fn record_engine_event(
        &self,
        name: cereyan_core::EventName,
        run_id: Option<i64>,
        flow_id: Option<i64>,
        payload: serde_json::Value,
    ) -> Result<i64, StoreError> {
        self.record_event(name.as_str(), run_id, flow_id, payload)
    }

    pub fn record_event(
        &self,
        name: &str,
        run_id: Option<i64>,
        flow_id: Option<i64>,
        payload: serde_json::Value,
    ) -> Result<i64, StoreError> {
        let mut resource = cereyan_core::Resource {
            kind: "custom".into(),
            id: String::new(),
            name: name.into(),
        };
        let mut related = Vec::new();
        let mut payload = payload;
        if let Some(rid) = run_id {
            // Only the six values the event is built from. A whole `Run` would
            // mean a correlated task_counts aggregate and four decoded JSON
            // columns per event, on the path every recorded event takes.
            if let Ok(Some(run)) = self.store.run_event_context(rid) {
                resource = cereyan_core::Resource {
                    kind: "run".into(),
                    id: run.external_id.to_string(),
                    name: run.name.clone(),
                };
                related.push(cereyan_core::Resource {
                    kind: "flow".into(),
                    id: format!("{}/{}", run.project, run.flow_name),
                    name: run.flow_name.clone(),
                });
                for t in &run.tags {
                    related.push(cereyan_core::Resource {
                        kind: "tag".into(),
                        id: t.clone(),
                        name: t.clone(),
                    });
                }
                if let Some(obj) = payload.as_object_mut() {
                    obj.entry("project")
                        .or_insert(serde_json::Value::String(run.project.clone()));
                    obj.entry("state")
                        .or_insert(serde_json::Value::String(run.state_name.clone()));
                }
            }
        } else if let Some(fid) = flow_id {
            if let Ok(Some(flow)) = self.store.get_flow(fid) {
                resource = cereyan_core::Resource {
                    kind: "flow".into(),
                    id: format!("{}/{}", flow.project, flow.name),
                    name: flow.name.clone(),
                };
                if let Some(obj) = payload.as_object_mut() {
                    obj.entry("project")
                        .or_insert(serde_json::Value::String(flow.project.clone()));
                }
            }
        }
        self.record_event_full(cereyan_store::NewEvent {
            name: name.into(),
            run_id,
            flow_id,
            payload,
            resource,
            related,
        })
    }

    pub fn record_event_full(&self, event: cereyan_store::NewEvent) -> Result<i64, StoreError> {
        let (event, _) = self.store.append_event(event)?;
        let id = event.id;
        self.after_event(event);
        Ok(id)
    }

    /// Publish a stored event and evaluate rules against it.
    ///
    /// Takes the event as written rather than an id: the caller already has
    /// every column, so reading the row back would be a wasted round trip on
    /// the hottest path in the server.
    pub fn after_event(&self, event: Event) {
        self.stream.publish(
            "event.created",
            event.id.to_string(),
            serde_json::to_value(&event).unwrap_or_default(),
        );
        if let Some(me) = self.self_ref() {
            crate::rules::on_event(&me, event);
        }
    }

    /// Publish and evaluate an event known only by id.
    ///
    /// Only for events appended inside a writer transaction, which report ids
    /// rather than rows. Every other caller has the event in hand and should
    /// call `after_event`.
    pub fn after_event_id(&self, id: i64) {
        if let Ok(Some(event)) = self.store.get_event(id) {
            self.after_event(event);
        }
    }

    pub fn publish_schedule(&self, schedule_id: i64) {
        let data = self
            .scheduler
            .get(schedule_id)
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or_else(|| json!({"id": schedule_id, "deleted": true}));
        self.stream
            .publish("schedule.updated", schedule_id.to_string(), data);
    }

    /// Publish a freshly created run and emit its first event.
    pub fn run_created(&self, run: &Run) {
        self.publish_run(run);
        crate::events::emit_run_event(self, run);
    }

    pub fn publish_run(&self, run: &Run) {
        self.stream.publish(
            "run.updated",
            run.id.to_string(),
            serde_json::to_value(run).unwrap_or_default(),
        );
    }

    pub fn publish_task_run(&self, task_run: &TaskRun) {
        self.stream.publish(
            "task_run.updated",
            task_run.id.to_string(),
            serde_json::to_value(task_run).unwrap_or_default(),
        );
    }

    pub fn publish_logs(&self, run_id: i64, last_id: i64, count: usize) {
        self.stream.publish(
            "log.appended",
            run_id.to_string(),
            json!({"run_id": run_id, "last_id": last_id, "count": count}),
        );
    }

    /// First phase of shutdown: stop handing out work and wake long-polls.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.supervisor.notify.notify_waiters();
    }

    pub fn shutdown_cleanup(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        // Engines parked in a long poll were told to exit by the work route. One that was
        // between requests never saw that answer and would wait out its own idle timeout, so
        // signal the idle ones the supervisor still records. SIGTERM to an exiting process is
        // a no-op. Engines executing a run are left alone for restart adoption to reclaim.
        for pid in self.supervisor.idle_engine_pids() {
            crate::process::terminate(pid);
        }
        let _ = self.store.flush();
        let _ = std::fs::remove_file(self.discovery_path());
        if let Some(path) = &self.config.socket {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod dep_graph_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The dependent ids the graph records for one upstream. The graph's value
    /// also carries the highest priority among them; `priority` below reads that,
    /// and the tests that assert ids use this so neither half is checked by
    /// accident in the other's place.
    fn ids(
        g: &std::collections::HashMap<i64, std::sync::Arc<Dependents>>,
        upstream: i64,
    ) -> Vec<i64> {
        g.get(&upstream).map(|d| d.ids.to_vec()).unwrap_or_default()
    }

    /// The highest dependent priority the graph records for one upstream.
    fn priority(
        g: &std::collections::HashMap<i64, std::sync::Arc<Dependents>>,
        upstream: i64,
    ) -> i64 {
        g.get(&upstream).map(|d| d.max_priority).unwrap_or(0)
    }

    /// The same two, over the plain map `dep_graph_from_flows` returns, before
    /// `AppState::dep_graph` puts each value behind an `Arc`.
    fn ids_of(g: &std::collections::HashMap<i64, Dependents>, upstream: i64) -> Vec<i64> {
        g.get(&upstream).map(|d| d.ids.to_vec()).unwrap_or_default()
    }

    fn priority_of(g: &std::collections::HashMap<i64, Dependents>, upstream: i64) -> i64 {
        g.get(&upstream).map(|d| d.max_priority).unwrap_or(0)
    }

    fn flow(id: i64, project: &str, name: &str, options: serde_json::Value) -> cereyan_core::Flow {
        let options = match options {
            serde_json::Value::Object(m) => m,
            _ => unreachable!("options is built as an object"),
        };
        cereyan_core::Flow {
            id,
            external_id: cereyan_core::new_id(),
            project: project.into(),
            name: name.into(),
            module: "m".into(),
            source_dir: "/tmp".into(),
            description: None,
            tags: vec![],
            group: None,
            parameter_schema: serde_json::json!({}),
            options,
            error: None,
            created_at: 0,
            last_seen_at: 0,
            live: false,
        }
    }

    fn after(name: &str) -> serde_json::Value {
        serde_json::json!({ "after": { "flow": name } })
    }

    fn fan_in(names: &[&str]) -> serde_json::Value {
        serde_json::json!({ "after": { "flow": names[0], "flows": names } })
    }

    fn no_after() -> serde_json::Value {
        serde_json::json!({})
    }

    #[test]
    fn a_single_upstream_dependency_is_recorded() {
        let flows = vec![
            flow(1, "p", "etl", no_after()),
            flow(2, "p", "daily", after("etl")),
        ];
        let g = dep_graph_from_flows(&flows);
        assert_eq!(ids_of(&g, 1), vec![2]);
        assert!(!g.contains_key(&2), "a dependent is not an upstream here");
    }

    /// The bug this fixes: only `after.flow` was used, so a fan-in declaration
    /// registered under its first upstream alone.
    #[test]
    fn a_fan_in_dependency_is_recorded_under_every_upstream() {
        let flows = vec![
            flow(1, "p", "a", no_after()),
            flow(2, "p", "b", no_after()),
            flow(3, "p", "join", fan_in(&["a", "b"])),
        ];
        let g = dep_graph_from_flows(&flows);
        assert_eq!(ids_of(&g, 1), vec![3], "first upstream missing the edge");
        assert_eq!(ids_of(&g, 2), vec![3], "second upstream missing the edge");
    }

    #[test]
    fn a_repeated_upstream_is_listed_once() {
        // `flow` and `flows` both naming the same flow must not double the edge.
        let flows = vec![
            flow(1, "p", "a", no_after()),
            flow(2, "p", "join", fan_in(&["a", "a"])),
        ];
        let g = dep_graph_from_flows(&flows);
        assert_eq!(ids_of(&g, 1), vec![2], "dependent listed twice");
    }

    #[test]
    fn dependencies_stay_within_a_project() {
        let flows = vec![
            flow(1, "a", "etl", no_after()),
            flow(2, "b", "etl", no_after()),
            flow(3, "b", "daily", after("etl")),
        ];
        let g = dep_graph_from_flows(&flows);
        assert!(!g.contains_key(&1), "crossed into another project");
        assert_eq!(ids_of(&g, 2), vec![3]);
    }

    #[test]
    fn a_dependency_on_a_missing_upstream_is_skipped() {
        let flows = vec![flow(1, "p", "daily", after("ghost"))];
        assert!(dep_graph_from_flows(&flows).is_empty());
    }

    /// The invariant the dependent-triggering path now rests on.
    ///
    /// It used to read every flow in the upstream's project and keep those whose
    /// `after` declared this flow; it now asks the graph. This asserts the two
    /// produce the same ids in the same order, over a set covering every case the
    /// two could plausibly disagree on: several dependents, none, a fan-in over
    /// two upstreams, a repeated name, the bare-name shorthand, an unknown
    /// upstream, a name in another project, and a flow carrying an error.
    #[test]
    fn the_graph_agrees_with_scanning_the_project() {
        /// The old way: read the project's flows and filter by `depends_on`.
        fn by_scan(flows: &[cereyan_core::Flow], upstream: &cereyan_core::Flow) -> Vec<i64> {
            flows
                .iter()
                .filter(|f| f.project == upstream.project)
                .filter(|f| {
                    let opts = cereyan_core::FlowOptions::from_map(&f.options);
                    opts.after
                        .as_ref()
                        .is_some_and(|a| a.depends_on(&upstream.name))
                })
                .map(|f| f.id)
                .collect()
        }

        let mut errored = flow(9, "p", "broken", after("etl"));
        errored.error = Some("boom".into());
        let flows = vec![
            flow(1, "p", "etl", no_after()),
            flow(2, "p", "first", after("etl")),
            flow(3, "p", "second", after("etl")),
            flow(4, "p", "join", fan_in(&["etl", "first"])),
            flow(5, "p", "repeated", fan_in(&["etl", "etl"])),
            flow(6, "p", "ghosted", after("nobody")),
            flow(7, "q", "elsewhere", after("etl")),
            flow(8, "p", "lonely", no_after()),
            errored,
        ];
        let g = dep_graph_from_flows(&flows);

        // Every upstream, including ones with no dependents.
        for upstream in &flows {
            let scanned = by_scan(&flows, upstream);
            let from_graph = ids_of(&g, upstream.id);
            assert_eq!(
                from_graph, scanned,
                "the graph and the scan disagree for flow {} ({})",
                upstream.id, upstream.name
            );
        }

        // And the interesting case is actually populated, so the comparison above
        // is not passing because both sides are empty.
        assert_eq!(
            ids_of(&g, 1),
            vec![2, 3, 4, 5, 9],
            "everything that declared `after=etl`, in flow order"
        );
        assert_eq!(
            ids_of(&g, 2),
            vec![4],
            "the fan-in flow also depends on `first`, which is flow 2"
        );
        assert!(
            !g.contains_key(&4),
            "nothing declares `after=join`, so it has no dependents"
        );
        assert!(!g.contains_key(&6), "a ghosted dependency has no upstream");
        assert!(!g.contains_key(&7), "a cross-project name does not cross");
    }

    #[test]
    fn several_dependents_are_all_listed() {
        let flows = vec![
            flow(1, "p", "etl", no_after()),
            flow(2, "p", "a", after("etl")),
            flow(3, "p", "b", after("etl")),
        ];
        let g = dep_graph_from_flows(&flows);
        assert_eq!(ids_of(&g, 1), vec![2, 3]);
    }

    // ---- the recorded priority ---------------------------------------------

    /// A dependent's declared priority, carried through the `after` fixture.
    fn after_at(name: &str, priority: i64) -> serde_json::Value {
        serde_json::json!({ "after": { "flow": name }, "priority": priority })
    }

    #[test]
    fn the_recorded_priority_is_the_highest_among_the_dependents() {
        let flows = vec![
            flow(1, "p", "etl", no_after()),
            flow(2, "p", "low", after_at("etl", 1)),
            flow(3, "p", "high", after_at("etl", 9)),
            flow(4, "p", "middle", after_at("etl", 4)),
        ];
        let g = dep_graph_from_flows(&flows);
        assert_eq!(ids_of(&g, 1), vec![2, 3, 4], "the ids, in flow order");
        assert_eq!(
            priority_of(&g, 1),
            9,
            "the greatest declared priority among them"
        );
    }

    #[test]
    fn a_negative_priority_does_not_lose_to_an_undeclared_one() {
        // `FlowOptions.priority` is `#[serde(default)]`, so an undeclared priority
        // is 0 -- which the old `priority.max(opts.priority)` treated as 0 too.
        let flows = vec![
            flow(1, "p", "etl", no_after()),
            flow(2, "p", "negative", after_at("etl", -5)),
        ];
        let g = dep_graph_from_flows(&flows);
        assert_eq!(
            priority_of(&g, 1),
            0,
            "an undeclared priority counts as zero"
        );
    }

    #[test]
    fn the_priority_is_accumulated_across_a_flows_fan_in() {
        // One dependent naming two upstreams contributes its priority to both.
        let mut join = flow(3, "p", "join", no_after());
        join.options = serde_json::json!({
            "after": { "flow": "a", "flows": ["a", "b"] },
            "priority": 6,
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        let flows = vec![
            flow(1, "p", "a", no_after()),
            flow(2, "p", "b", no_after()),
            join,
        ];
        let g = dep_graph_from_flows(&flows);
        assert_eq!(ids_of(&g, 1), vec![3], "`join` is recorded under `a`");
        assert_eq!(ids_of(&g, 2), vec![3], "and under `b`");
        assert_eq!(
            priority_of(&g, 1),
            6,
            "the same dependent contributes its own priority to each upstream"
        );
        assert_eq!(priority_of(&g, 2), 6);

        // And a second dependent of `a` at a lower priority does not lower it.
        let mut low = flow(4, "p", "low", no_after());
        low.options = serde_json::json!({ "after": { "flow": "a" }, "priority": 2 })
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut all = flows;
        all.push(low);
        let g = dep_graph_from_flows(&all);
        assert_eq!(ids_of(&g, 1), vec![3, 4]);
        assert_eq!(priority_of(&g, 1), 6, "the maximum, not the last one seen");
        assert_eq!(priority_of(&g, 2), 6, "`b` has only the one dependent");
    }

    #[test]
    fn the_ids_and_the_priority_come_from_the_same_dependents() {
        // The two halves must describe the same set, or the graph would report a
        // priority declared by a flow it does not list.
        let flows = vec![
            flow(1, "p", "etl", no_after()),
            flow(2, "p", "a", after_at("etl", 5)),
            flow(3, "p", "b", no_after()),
            flow(4, "p", "c", after_at("etl", 2)),
        ];
        let g = dep_graph_from_flows(&flows);
        let listed = ids_of(&g, 1);
        assert_eq!(listed, vec![2, 4], "`b` declares no dependency");
        assert_eq!(priority_of(&g, 1), 5, "and so cannot have set the maximum");
    }

    #[test]
    fn a_flow_with_no_dependents_records_no_priority() {
        let flows = vec![
            flow(1, "p", "etl", no_after()),
            flow(2, "p", "daily", after_at("etl", 9)),
        ];
        let g = dep_graph_from_flows(&flows);
        assert!(!g.contains_key(&2), "`daily` has no dependents");
        assert_eq!(priority_of(&g, 2), 0, "so it has no priority either");
    }

    #[test]
    fn the_cache_hands_back_the_recorded_priority() {
        let cache = DepGraphCache::default();
        let g = cache.get_or_build(|| sample(1));
        assert_eq!(ids(&g, 1), vec![1]);
        assert_eq!(priority(&g, 1), 7, "the priority survives the cache");
    }

    #[test]
    fn an_empty_flow_list_yields_an_empty_graph() {
        assert!(dep_graph_from_flows(&[]).is_empty());
    }

    // ---- the cache itself -------------------------------------------------

    fn sample(id: i64) -> DepGraph {
        std::sync::Arc::new(
            [(
                id,
                std::sync::Arc::new(Dependents {
                    ids: vec![id].into_boxed_slice(),
                    max_priority: 7,
                }),
            )]
            .into_iter()
            .collect(),
        )
    }

    #[test]
    fn a_warm_cache_never_rebuilds() {
        let cache = DepGraphCache::default();
        let builds = AtomicUsize::new(0);
        for _ in 0..5 {
            let g = cache.get_or_build(|| {
                builds.fetch_add(1, Ordering::SeqCst);
                sample(1)
            });
            assert_eq!(ids(&g, 1), vec![1]);
        }
        assert_eq!(builds.load(Ordering::SeqCst), 1, "rebuilt on a warm cache");
    }

    /// An invalidation that lands while a build reads the flows: the graph that
    /// build made predates the change, so it is handed to its caller but not
    /// cached, and the next lookup builds afresh.
    #[test]
    fn a_graph_built_across_an_invalidation_is_not_cached() {
        let cache = DepGraphCache::default();
        let g = cache.get_or_build(|| {
            cache.invalidate();
            sample(1)
        });
        assert_eq!(ids(&g, 1), vec![1]);
        assert!(!cache.is_populated(), "the stale graph was cached");
        let g = cache.get_or_build(|| sample(2));
        assert_eq!(ids(&g, 2), vec![2], "the next lookup rebuilt");
    }

    /// A failed build caches nothing, so the next lookup tries again.
    #[test]
    fn a_failed_build_is_not_cached() {
        let cache = DepGraphCache::default();
        assert!(cache.try_get_or_build(|| None).is_empty());
        assert!(!cache.is_populated());
        let g = cache.try_get_or_build(|| Some(sample(3)));
        assert_eq!(ids(&g, 3), vec![3]);
    }

    #[test]
    fn invalidating_forces_one_rebuild() {
        let cache = DepGraphCache::default();
        let builds = AtomicUsize::new(0);
        let build = || {
            builds.fetch_add(1, Ordering::SeqCst);
            sample(1)
        };
        cache.get_or_build(build);
        assert!(cache.is_populated());
        cache.invalidate();
        assert!(!cache.is_populated(), "invalidate left the graph in place");
        cache.get_or_build(build);
        cache.get_or_build(build);
        assert_eq!(
            builds.load(Ordering::SeqCst),
            2,
            "expected exactly one rebuild"
        );
    }

    #[test]
    fn the_graph_is_shared_not_copied() {
        let cache = DepGraphCache::default();
        let a = cache.get_or_build(|| sample(7));
        let b = cache.get_or_build(|| sample(7));
        assert!(
            std::sync::Arc::ptr_eq(&a, &b),
            "a warm lookup returned a different Arc, so the graph was copied"
        );
    }

    /// The double check means one build wins, not one per racing thread.
    #[test]
    fn concurrent_lookups_after_invalidation_build_once() {
        let cache = std::sync::Arc::new(DepGraphCache::default());
        let builds = std::sync::Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let cache = std::sync::Arc::clone(&cache);
            let builds = std::sync::Arc::clone(&builds);
            handles.push(std::thread::spawn(move || {
                cache.get_or_build(|| {
                    builds.fetch_add(1, Ordering::SeqCst);
                    // Widen the window a racing thread could slip through.
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    sample(1)
                })
            }));
        }
        let graphs: Vec<DepGraph> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "more than one thread built the graph"
        );
        for g in &graphs {
            assert!(std::sync::Arc::ptr_eq(g, &graphs[0]), "threads disagreed");
        }
    }
}

#[cfg(test)]
mod transition_tests {
    use super::*;
    use tempfile::TempDir;

    /// The run a transition returns carries the timing and counters the write
    /// updated, the same as reading the row back would.
    #[test]
    fn a_transition_returns_the_updated_timing_and_counters() {
        let dir = TempDir::new().unwrap();
        let state =
            crate::api::flows::list_flows_tests::state_with_flows(&dir, &[("p", "f", None, None)]);
        let flow_id = state.store.list_flows(None).unwrap()[0].id;
        let (run_id, _) = state
            .store
            .create_run_full(cereyan_store::CreateRun {
                flow_id,
                name: "r".into(),
                parameters: "{}".into(),
                tags: "[]".into(),
                created_by: "test".into(),
                ..Default::default()
            })
            .unwrap();
        for next in [StateType::Pending, StateType::Running, StateType::Failed] {
            let TransitionResult::Accepted(run) = state
                .transition_run(run_id, State::new(next), false)
                .unwrap()
            else {
                panic!("{next:?} was rejected");
            };
            let stored = state.store.get_run(run_id).unwrap().unwrap();
            assert_eq!(
                run.start_time, stored.start_time,
                "start_time after {next:?}"
            );
            assert_eq!(run.end_time, stored.end_time, "end_time after {next:?}");
            assert_eq!(
                run.total_run_time, stored.total_run_time,
                "total_run_time after {next:?}"
            );
            assert_eq!(
                run.failure_count, stored.failure_count,
                "failure_count after {next:?}"
            );
            assert_eq!(
                run.crash_count, stored.crash_count,
                "crash_count after {next:?}"
            );
        }
        let run = state.store.get_run(run_id).unwrap().unwrap();
        assert!(run.end_time.is_some() && run.failure_count == 1);
    }
}
