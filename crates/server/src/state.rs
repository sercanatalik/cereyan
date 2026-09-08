//! Shared server state and the transition path used by every writer.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use cereyan_core::{now_micros, Run, State, StateType, TaskRun};
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
    pub retain_days: std::sync::atomic::AtomicI64,
    pub crash_retries_default: std::sync::atomic::AtomicI64,
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
        addr: SocketAddr,
        shutdown: watch::Receiver<bool>,
    ) -> Result<AppState, ServerError> {
        let live: HashSet<i64> = config.live_flows.iter().copied().collect();
        let retain_days = config.retain_days;
        let crash_retries_default = config.crash_retries_default;
        let state = AppState {
            supervisor: Supervisor::new(&config),
            mcp: crate::mcp::Sessions::default(),
            scheduler: Scheduler::new(),
            timer: Timer::new(),
            config,
            store,
            index: ActiveIndex::new(),
            stream: Broadcaster::new(),
            dispatcher,
            live_flows: RwLock::new(live),
            addr,
            started_at: now_micros(),
            shutting_down: AtomicBool::new(false),
            shutdown,
            weak: RwLock::new(None),
            rules: crate::rules::RulesState::default(),
            rule_dispatcher: RwLock::new(None),
            retain_days: std::sync::atomic::AtomicI64::new(retain_days),
            crash_retries_default: std::sync::atomic::AtomicI64::new(crash_retries_default),
        };
        Ok(state)
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

    pub fn write_discovery_file(&self) -> Result<(), ServerError> {
        // `host` records what was bound; `url` has to be dialable. A listener on an
        // unspecified address answers on loopback, and 0.0.0.0 is not somewhere a
        // client can connect: Linux and macOS route it to loopback, Windows refuses
        // it outright, so every discovery consumer there fails to find the server.
        let url = if self.addr.ip().is_unspecified() {
            let loopback = if self.addr.is_ipv6() {
                "[::1]"
            } else {
                "127.0.0.1"
            };
            format!("http://{}:{}", loopback, self.addr.port())
        } else {
            format!("http://{}", self.addr)
        };
        let body = json!({
            "host": self.addr.ip().to_string(),
            "port": self.addr.port(),
            "url": url,
            "pid": std::process::id(),
            "started_at": self.started_at,
            "version": self.config.version,
            "auth": self.config.token.is_some(),
            "socket": self.config.socket.as_ref().map(|p| p.display().to_string()),
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
            flows.iter().map(|f| (f.id, f.project.clone())).collect(),
        );
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
                    if run
                        .scheduled_time
                        .map(|t| t <= now_micros())
                        .unwrap_or(true)
                    {
                        self.supervisor.enqueue_simple(run.id, key);
                    }
                }
                _ => {
                    let alive = run
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
        match self.store.transition_run(run_id, state, force) {
            Ok(_) => {}
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
        }
        let run = self
            .store
            .get_run(run_id)?
            .ok_or(StoreError::NotFound("run"))?;
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
        if run.state.state_type == StateType::Cancelled {
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
            if let Ok(Some(run)) = self.store.get_run(rid) {
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
                        .or_insert(serde_json::Value::String(run.state.name.clone()));
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
        let (id, _) = self.store.append_event(event)?;
        self.after_event(id);
        Ok(id)
    }

    /// Publish a stored event and evaluate rules against it.
    pub fn after_event(&self, id: i64) {
        if let Ok(Some(event)) = self.store.get_event(id) {
            self.stream.publish(
                "event.created",
                id.to_string(),
                serde_json::to_value(&event).unwrap_or_default(),
            );
            if let Some(me) = self.self_ref() {
                crate::rules::on_event(&me, event);
            }
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
