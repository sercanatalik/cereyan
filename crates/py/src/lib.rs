//! `cereyan._core`: the pyo3 module. Store calls release the GIL; JSON strings
//! cross the boundary so Python never sees Rust structs.

mod client;
mod server;

use std::sync::Arc;

use cereyan_core::{State, StateType};
use cereyan_store::{ListRunsFilter, NewLog, StoreError};
use pyo3::create_exception;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

create_exception!(
    _core,
    StoreLocked,
    PyRuntimeError,
    "The store is locked by another process."
);
create_exception!(
    _core,
    TransitionRejected,
    PyRuntimeError,
    "A state transition was rejected by the rules."
);

fn to_py(e: StoreError) -> PyErr {
    match e {
        StoreError::Locked { .. } => StoreLocked::new_err(e.to_string()),
        StoreError::Rejected(reason) => TransitionRejected::new_err(reason),
        StoreError::RejectedWith { reason, .. } => TransitionRejected::new_err(reason),
        StoreError::Invalid(_) => PyValueError::new_err(e.to_string()),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

fn parse_state(
    state_type: &str,
    name: Option<&str>,
    message: Option<String>,
    details: Option<&str>,
) -> PyResult<State> {
    let t = StateType::parse(state_type)
        .ok_or_else(|| PyValueError::new_err(format!("unknown state type {state_type:?}")))?;
    let details = match details {
        Some(d) if !d.is_empty() => serde_json::from_str(d)
            .map_err(|e| PyValueError::new_err(format!("details is not a JSON object: {e}")))?,
        _ => Default::default(),
    };
    Ok(State::from_parts(t, name, message, details))
}

fn json<T: serde::Serialize>(v: &T) -> PyResult<String> {
    serde_json::to_string(v).map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

#[pyclass(frozen)]
#[allow(clippy::too_many_arguments)]
pub struct Store {
    pub(crate) inner: Arc<cereyan_store::Store>,
}

#[pymethods]
#[allow(clippy::too_many_arguments)]
impl Store {
    /// Open the store. `home` of None resolves through CEREYAN_HOME then ~/.cereyan.
    #[staticmethod]
    #[pyo3(signature = (home=None))]
    fn open(py: Python<'_>, home: Option<String>) -> PyResult<Store> {
        let inner = py.detach(move || {
            let path = cereyan_store::resolve_home(home.as_deref().map(std::path::Path::new));
            cereyan_store::Store::open(&path).map(Arc::new)
        });
        Ok(Store {
            inner: inner.map_err(to_py)?,
        })
    }

    /// The resolved home directory.
    #[staticmethod]
    #[pyo3(signature = (home=None))]
    fn resolve_home(home: Option<String>) -> String {
        cereyan_store::resolve_home(home.as_deref().map(std::path::Path::new))
            .display()
            .to_string()
    }

    #[getter]
    fn home(&self) -> String {
        self.inner.home().display().to_string()
    }

    #[pyo3(signature = (project, name, module, source_dir, description=None, tags=String::from("[]"), parameter_schema=String::from("{}"), options=String::from("{}")))]
    #[allow(clippy::too_many_arguments)]
    fn upsert_flow(
        &self,
        py: Python<'_>,
        project: String,
        name: String,
        module: String,
        source_dir: String,
        description: Option<String>,
        tags: String,
        parameter_schema: String,
        options: String,
    ) -> PyResult<i64> {
        let store = self.inner.clone();
        py.detach(move || {
            store.upsert_flow_full(cereyan_store::UpsertFlow {
                project,
                name,
                module,
                source_dir,
                description,
                tags,
                parameter_schema,
                options,
            })
        })
        .map_err(to_py)
    }

    #[pyo3(signature = (flow_id))]
    fn get_flow(&self, py: Python<'_>, flow_id: i64) -> PyResult<Option<String>> {
        let store = self.inner.clone();
        let flow = py.detach(move || store.get_flow(flow_id)).map_err(to_py)?;
        flow.map(|f| json(&f)).transpose()
    }

    fn set_flow_error(&self, py: Python<'_>, flow_id: i64, error: Option<String>) -> PyResult<()> {
        let store = self.inner.clone();
        py.detach(move || store.set_flow_error(flow_id, error))
            .map_err(to_py)
    }

    /// Returns (run_id, external_id).
    #[pyo3(signature = (flow_id, name, parameters=String::from("{}"), tags=String::from("[]")))]
    fn create_run(
        &self,
        py: Python<'_>,
        flow_id: i64,
        name: String,
        parameters: String,
        tags: String,
    ) -> PyResult<(i64, String)> {
        let store = self.inner.clone();
        let (id, ext) = py
            .detach(move || store.create_run(flow_id, &name, &parameters, &tags))
            .map_err(to_py)?;
        Ok((id, ext.to_string()))
    }

    /// Propose a run state. Returns the accepted state as JSON or raises TransitionRejected.
    #[pyo3(signature = (run_id, state_type, name=None, message=None, details=None, force=false))]
    fn transition(
        &self,
        py: Python<'_>,
        run_id: i64,
        state_type: &str,
        name: Option<&str>,
        message: Option<String>,
        details: Option<&str>,
        force: bool,
    ) -> PyResult<String> {
        let state = parse_state(state_type, name, message, details)?;
        let store = self.inner.clone();
        let accepted = py
            .detach(move || store.transition_run(run_id, state, force))
            .map_err(to_py)?;
        json(&accepted)
    }

    /// Returns (task_run_id, external_id).
    #[pyo3(signature = (run_id, name, task_key, dynamic_key, parents=None))]
    fn create_task_run(
        &self,
        py: Python<'_>,
        run_id: i64,
        name: String,
        task_key: String,
        dynamic_key: String,
        parents: Option<Vec<String>>,
    ) -> PyResult<(i64, String)> {
        let store = self.inner.clone();
        let parents: Vec<cereyan_core::Id> = parents
            .unwrap_or_default()
            .iter()
            .filter_map(|p| cereyan_core::Id::parse(p))
            .collect();
        let (id, ext) = py
            .detach(move || {
                store.create_task_run_full(cereyan_store::CreateTaskRun {
                    run_id,
                    name,
                    task_key,
                    dynamic_key,
                    external_id: None,
                    parents,
                })
            })
            .map_err(to_py)?;
        Ok((id, ext.to_string()))
    }

    #[pyo3(signature = (task_run_id, state_type, name=None, message=None, details=None, force=false))]
    fn transition_task_run(
        &self,
        py: Python<'_>,
        task_run_id: i64,
        state_type: &str,
        name: Option<&str>,
        message: Option<String>,
        details: Option<&str>,
        force: bool,
    ) -> PyResult<String> {
        let state = parse_state(state_type, name, message, details)?;
        let store = self.inner.clone();
        let accepted = py
            .detach(move || store.transition_task_run(task_run_id, state, force))
            .map_err(to_py)?;
        json(&accepted)
    }

    /// Append log records given as (run_id, task_run_id, level, logger, timestamp_micros, message).
    fn append_logs(
        &self,
        py: Python<'_>,
        logs: Vec<(i64, Option<i64>, i32, String, i64, String)>,
    ) -> PyResult<usize> {
        let logs: Vec<NewLog> = logs
            .into_iter()
            .map(
                |(run_id, task_run_id, level, logger, timestamp, message)| NewLog {
                    run_id,
                    task_run_id,
                    task_run_external_id: None,
                    level,
                    logger,
                    timestamp,
                    message,
                },
            )
            .collect();
        let store = self.inner.clone();
        py.detach(move || store.append_logs(logs)).map_err(to_py)
    }

    /// List runs. `filter` is a JSON object with optional project, flow, flow_id,
    /// state_type, state_name, name, limit, cursor. Returns {"items": [...], "next_cursor": ...}.
    #[pyo3(signature = (filter="{}"))]
    fn list_runs(&self, py: Python<'_>, filter: &str) -> PyResult<String> {
        let filter: ListRunsFilter = serde_json::from_str(filter)
            .map_err(|e| PyValueError::new_err(format!("bad filter: {e}")))?;
        let store = self.inner.clone();
        let page = py.detach(move || store.list_runs(&filter)).map_err(to_py)?;
        json(&page)
    }

    /// Get one run by integer id; returns JSON or None.
    fn get_run(&self, py: Python<'_>, run_id: i64) -> PyResult<Option<String>> {
        let store = self.inner.clone();
        let run = py.detach(move || store.get_run(run_id)).map_err(to_py)?;
        run.map(|r| json(&r)).transpose()
    }

    fn task_runs(&self, py: Python<'_>, run_id: i64) -> PyResult<String> {
        let store = self.inner.clone();
        let rows = py
            .detach(move || store.task_runs_by_run(run_id))
            .map_err(to_py)?;
        json(&rows)
    }

    #[pyo3(signature = (run_id, after_id=0, limit=1000))]
    fn logs(&self, py: Python<'_>, run_id: i64, after_id: i64, limit: usize) -> PyResult<String> {
        let store = self.inner.clone();
        let rows = py
            .detach(move || store.logs_by_run(run_id, after_id, limit))
            .map_err(to_py)?;
        json(&rows)
    }

    /// Query logs with a JSON filter (run_id, task_run_id, after_id, min_level, search, limit).
    #[pyo3(signature = (filter="{}"))]
    fn query_logs(&self, py: Python<'_>, filter: &str) -> PyResult<String> {
        let filter: cereyan_store::LogFilter = serde_json::from_str(filter)
            .map_err(|e| PyValueError::new_err(format!("bad filter: {e}")))?;
        let store = self.inner.clone();
        let page = py.detach(move || store.logs(&filter)).map_err(to_py)?;
        json(&page)
    }

    #[pyo3(signature = (project=None))]
    fn list_flows(&self, py: Python<'_>, project: Option<String>) -> PyResult<String> {
        let store = self.inner.clone();
        let rows = py
            .detach(move || store.list_flows(project.as_deref()))
            .map_err(to_py)?;
        json(&rows)
    }

    /// Record an event. `resource` and `related` are JSON; the run's resource is
    /// filled in when `run_id` is given and `resource` is None.
    #[pyo3(signature = (name, payload="{}", run_id=None, flow_id=None, resource=None, task_run_external_id=None))]
    fn append_event(
        &self,
        py: Python<'_>,
        name: String,
        payload: &str,
        run_id: Option<i64>,
        flow_id: Option<i64>,
        resource: Option<&str>,
        task_run_external_id: Option<String>,
    ) -> PyResult<i64> {
        let payload: serde_json::Value = serde_json::from_str(payload)
            .map_err(|e| PyValueError::new_err(format!("payload is not JSON: {e}")))?;
        let explicit: Option<cereyan_core::Resource> = match resource {
            Some(r) => Some(
                serde_json::from_str(r)
                    .map_err(|e| PyValueError::new_err(format!("resource is not JSON: {e}")))?,
            ),
            None => None,
        };
        let store = self.inner.clone();
        py.detach(move || {
            let mut resource = explicit.unwrap_or(cereyan_core::Resource {
                kind: "custom".into(),
                id: String::new(),
                name: name.clone(),
            });
            let mut related = Vec::new();
            let mut flow_id = flow_id;
            if let Some(rid) = run_id {
                if let Ok(Some(run)) = store.get_run(rid) {
                    if resource.kind == "custom" {
                        resource = cereyan_core::Resource {
                            kind: "run".into(),
                            id: run.external_id.to_string(),
                            name: run.name.clone(),
                        };
                    }
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
                    flow_id = flow_id.or(Some(run.flow_id));
                }
            }
            if let Some(ext) = task_run_external_id {
                related.push(cereyan_core::Resource {
                    kind: "task_run".into(),
                    id: ext,
                    name: String::new(),
                });
            }
            store
                .append_event(cereyan_store::NewEvent {
                    name,
                    run_id,
                    flow_id,
                    payload,
                    resource,
                    related,
                })
                .map(|(id, _)| id)
        })
        .map_err(to_py)
    }

    /// Query events with a JSON filter (see EventFilter). Returns a page as JSON.
    #[pyo3(signature = (filter="{}"))]
    fn query_events(&self, py: Python<'_>, filter: &str) -> PyResult<String> {
        let filter: cereyan_store::EventFilter = serde_json::from_str(filter)
            .map_err(|e| PyValueError::new_err(format!("bad filter: {e}")))?;
        let store = self.inner.clone();
        let page = py
            .detach(move || store.query_events(&filter))
            .map_err(to_py)?;
        json(&page)
    }

    #[pyo3(signature = (run_id, kind, data, key=None, task_run_id=None))]
    fn upsert_artifact(
        &self,
        py: Python<'_>,
        run_id: i64,
        kind: String,
        data: String,
        key: Option<String>,
        task_run_id: Option<i64>,
    ) -> PyResult<i64> {
        let store = self.inner.clone();
        py.detach(move || {
            store.upsert_artifact(cereyan_store::UpsertArtifact {
                run_id,
                task_run_id,
                kind,
                key,
                data,
                external_id: None,
            })
        })
        .map_err(to_py)
    }

    fn artifacts(&self, py: Python<'_>, run_id: i64) -> PyResult<String> {
        let store = self.inner.clone();
        let rows = py
            .detach(move || store.artifacts_by_run(run_id))
            .map_err(to_py)?;
        json(&rows)
    }

    /// Set a variable; secrets are encrypted with the home's key.
    #[pyo3(signature = (name, value, tags=String::from("[]"), secret=false))]
    fn set_variable(
        &self,
        py: Python<'_>,
        name: String,
        value: String,
        tags: String,
        secret: bool,
    ) -> PyResult<()> {
        let store = self.inner.clone();
        let home = store.home().to_path_buf();
        py.detach(move || {
            let stored = if secret {
                cereyan_store::secrets::encrypt(&home, &value)
                    .map_err(|e| cereyan_store::StoreError::Invalid(e.to_string()))?
            } else {
                value
            };
            store.set_variable(&name, &stored, &tags, secret)
        })
        .map_err(to_py)
    }

    /// Get a variable's plain JSON text (secrets decrypted) or None.
    fn get_variable(&self, py: Python<'_>, name: String) -> PyResult<Option<String>> {
        let store = self.inner.clone();
        let home = store.home().to_path_buf();
        let result: Result<Option<String>, String> =
            py.detach(
                move || match store.get_variable(&name).map_err(|e| e.to_string())? {
                    None => Ok(None),
                    Some((row, raw)) => {
                        if row.secret {
                            cereyan_store::secrets::decrypt(&home, &raw)
                                .map(Some)
                                .map_err(|e| e.to_string())
                        } else {
                            Ok(Some(raw))
                        }
                    }
                },
            );
        result.map_err(PyRuntimeError::new_err)
    }

    fn delete_variable(&self, py: Python<'_>, name: String) -> PyResult<bool> {
        let store = self.inner.clone();
        py.detach(move || store.delete_variable(&name))
            .map_err(to_py)
    }

    fn list_variables(&self, py: Python<'_>) -> PyResult<String> {
        let store = self.inner.clone();
        let rows = py.detach(move || store.list_variables()).map_err(to_py)?;
        json(&rows)
    }

    fn list_rules(&self, py: Python<'_>) -> PyResult<String> {
        let store = self.inner.clone();
        let rows = py.detach(move || store.list_rules()).map_err(to_py)?;
        json(&rows)
    }

    /// Insert or update a rule row; `spec` is the JSON RuleSpec.
    #[pyo3(signature = (name, spec, source="code", module=None, id=None, enabled=true))]
    fn upsert_rule(
        &self,
        py: Python<'_>,
        name: String,
        spec: String,
        source: &str,
        module: Option<String>,
        id: Option<i64>,
        enabled: bool,
    ) -> PyResult<i64> {
        let source = source.to_string();
        let store = self.inner.clone();
        py.detach(move || {
            store.upsert_rule(cereyan_store::RuleWrite {
                id,
                name,
                enabled,
                source,
                module,
                spec,
            })
        })
        .map_err(to_py)
    }

    fn prune_code_rules(&self, py: Python<'_>, keep: Vec<i64>) -> PyResult<usize> {
        let store = self.inner.clone();
        py.detach(move || store.prune_code_rules(keep))
            .map_err(to_py)
    }

    #[pyo3(signature = (rule_id, event_id=None, run_id=None, outcomes="[]"))]
    fn record_firing(
        &self,
        py: Python<'_>,
        rule_id: i64,
        event_id: Option<i64>,
        run_id: Option<i64>,
        outcomes: &str,
    ) -> PyResult<i64> {
        let outcomes = outcomes.to_string();
        let store = self.inner.clone();
        py.detach(move || store.record_firing(rule_id, event_id, run_id, &outcomes))
            .map_err(to_py)
    }

    fn run_name_exists(&self, py: Python<'_>, name: String) -> PyResult<bool> {
        let store = self.inner.clone();
        py.detach(move || store.run_name_exists(&name))
            .map_err(to_py)
    }

    fn delete_run(&self, py: Python<'_>, run_id: i64) -> PyResult<bool> {
        let store = self.inner.clone();
        py.detach(move || store.delete_run(run_id)).map_err(to_py)
    }

    /// Wait for every queued write to commit.
    fn flush(&self, py: Python<'_>) -> PyResult<()> {
        let store = self.inner.clone();
        py.detach(move || store.flush()).map_err(to_py)
    }
}

/// Decrypt a secret ciphertext with the key in `home`.
#[pyfunction]
fn decrypt_secret(home: &str, text: &str) -> PyResult<String> {
    cereyan_store::secrets::decrypt(std::path::Path::new(home), text)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

/// Render a rule action's templates against a JSON context (offline rules).
#[pyfunction]
fn render_rule_action(action: &str, context: &str) -> PyResult<String> {
    let action: cereyan_core::RuleAction =
        serde_json::from_str(action).map_err(|e| PyValueError::new_err(format!("action: {e}")))?;
    let ctx: serde_json::Value = serde_json::from_str(context)
        .map_err(|e| PyValueError::new_err(format!("context: {e}")))?;
    let env = cereyan_rules::environment();
    let rendered = cereyan_rules::render_action(&env, &action, &ctx)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    json(&rendered)
}

/// Does a rule match clause accept an event? Both are JSON; `run` may be null.
#[pyfunction]
fn rule_matches(when: &str, event: &str, run: &str) -> PyResult<bool> {
    let m: cereyan_core::RuleMatch =
        serde_json::from_str(when).map_err(|e| PyValueError::new_err(format!("when: {e}")))?;
    let e: cereyan_core::Event =
        serde_json::from_str(event).map_err(|e| PyValueError::new_err(format!("event: {e}")))?;
    let run: Option<cereyan_core::Run> = serde_json::from_str(run).ok();
    let ctx = cereyan_rules::RunContext { run, flow: None };
    Ok(cereyan_rules::matches(&m, &e, &ctx))
}

/// Current time in microseconds since the Unix epoch, UTC.
#[pyfunction]
fn now_micros() -> i64 {
    cereyan_core::now_micros()
}

/// Generate a UUIDv7 as a string.
#[pyfunction]
fn new_id() -> String {
    cereyan_core::new_id().to_string()
}

#[pyfunction]
fn state_types() -> Vec<&'static str> {
    StateType::ALL.iter().map(|t| t.as_str()).collect()
}

#[pyfunction]
fn state_names() -> Vec<(&'static str, &'static str)> {
    cereyan_core::StateName::ALL
        .iter()
        .map(|n| (n.as_str(), n.state_type().as_str()))
        .collect()
}

#[pyfunction]
fn is_terminal(state_type: &str) -> PyResult<bool> {
    StateType::parse(state_type)
        .map(|t| t.is_terminal())
        .ok_or_else(|| PyValueError::new_err(format!("unknown state type {state_type:?}")))
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Store>()?;
    m.add_class::<server::Server>()?;
    m.add_class::<client::Client>()?;
    m.add_function(wrap_pyfunction!(now_micros, m)?)?;
    m.add_function(wrap_pyfunction!(new_id, m)?)?;
    m.add_function(wrap_pyfunction!(state_types, m)?)?;
    m.add_function(wrap_pyfunction!(state_names, m)?)?;
    m.add_function(wrap_pyfunction!(is_terminal, m)?)?;
    m.add_function(wrap_pyfunction!(decrypt_secret, m)?)?;
    m.add_function(wrap_pyfunction!(render_rule_action, m)?)?;
    m.add_function(wrap_pyfunction!(rule_matches, m)?)?;
    m.add("StoreLocked", m.py().get_type::<StoreLocked>())?;
    m.add(
        "TransitionRejected",
        m.py().get_type::<TransitionRejected>(),
    )?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
