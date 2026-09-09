//! Engine-side HTTP client and batched reporter. Task transitions and logs
//! are buffered here and flushed every 100 ms or on demand over a keep-alive
//! connection; heartbeats ride the same thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cereyan_core::{Id, State, StateType};
use cereyan_store::{NewLog, ReportEvent};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use serde_json::{json, Value};

const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(600);
/// An idle engine (no run in progress) stops waiting for a server sooner.
const IDLE_RETRY: Duration = Duration::from_secs(30);
/// How long a work long-poll may take before the client gives up on it.
///
/// This is the real bound on an idle engine's exit, not IDLE_RETRY. The elapsed
/// check runs in the error arm of the request loop, so it is only reached once a
/// request has returned: a request that hangs holds the loop for this long before
/// anything looks at the clock. The server holds a work request for `wait_ms`
/// (30 s), so this only has to outlast that with margin — a larger value buys
/// nothing and pushes the give-up out by the difference.
const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(40);
const MAX_BUFFERED_LOGS: usize = 1_000_000;

/// Items (events, or log lines inside a Logs event) per report request.
const MAX_BATCH_ITEMS: usize = 5_000;

#[derive(Default)]
struct RunBuffer {
    next_seq: i64,
    pending: Vec<ReportEvent>,
    logs: Vec<NewLog>,
    last_contact: Option<Instant>,
    cancel: bool,
    dropped_logs: usize,
}

impl RunBuffer {
    fn seal_logs(&mut self) {
        // Split long log runs into bounded events so a request stays small.
        while !self.logs.is_empty() {
            let take = self.logs.len().min(MAX_BATCH_ITEMS);
            let rest = self.logs.split_off(take);
            let logs = std::mem::replace(&mut self.logs, rest);
            self.next_seq += 1;
            self.pending.push(ReportEvent::Logs {
                seq: self.next_seq,
                logs,
            });
        }
    }

    /// Events for the next request: whole events until the item budget fills.
    fn next_chunk(&self) -> Vec<ReportEvent> {
        let mut items = 0usize;
        let mut count = 0usize;
        for e in &self.pending {
            let size = match e {
                ReportEvent::Logs { logs, .. } => logs.len().max(1),
                _ => 1,
            };
            if count > 0 && items + size > MAX_BATCH_ITEMS {
                break;
            }
            items += size;
            count += 1;
        }
        self.pending[..count].to_vec()
    }
}

struct Inner {
    base: String,
    engine_id: String,
    /// Bearer token sent on every request when the server requires one.
    token: Option<String>,
    agent: ureq::Agent,
    long_agent: ureq::Agent,
    runs: Mutex<HashMap<i64, RunBuffer>>,
    stop: AtomicBool,
    last_failure_log: Mutex<Option<Instant>>,
}

impl Inner {
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.trim_end_matches('/'), path)
    }

    fn post_json(
        &self,
        agent: &ureq::Agent,
        path: &str,
        body: &Value,
    ) -> Result<(u16, Value), String> {
        let mut req = agent.post(&self.url(path));
        if let Some(t) = &self.token {
            req = req.header("authorization", &format!("Bearer {t}"));
        }
        let mut resp = req.send_json(body).map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let value: Value = resp.body_mut().read_json::<Value>().unwrap_or(Value::Null);
        Ok((status, value))
    }

    fn get_json(&self, path: &str) -> Result<(u16, Value), String> {
        let mut req = self.agent.get(&self.url(path));
        if let Some(t) = &self.token {
            req = req.header("authorization", &format!("Bearer {t}"));
        }
        let mut resp = req.call().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let value: Value = resp.body_mut().read_json::<Value>().unwrap_or(Value::Null);
        Ok((status, value))
    }

    /// POST with reconnection backoff for the server-restart case.
    fn post_retry(
        &self,
        path: &str,
        body: &Value,
        max_wait: Duration,
    ) -> Result<(u16, Value), String> {
        let start = Instant::now();
        let mut delay = Duration::from_millis(200);
        loop {
            match self.post_json(&self.agent, path, body) {
                Ok(ok) => return Ok(ok),
                Err(e) => {
                    if self.stop.load(Ordering::SeqCst) || start.elapsed() > max_wait {
                        return Err(e);
                    }
                    self.log_failure(&e);
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_secs(5));
                }
            }
        }
    }

    fn log_failure(&self, e: &str) {
        let mut last = self
            .last_failure_log
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if last
            .map(|t| t.elapsed() > Duration::from_secs(10))
            .unwrap_or(true)
        {
            eprintln!("cereyan engine: server unreachable ({e}); retrying");
            *last = Some(Instant::now());
        }
    }

    /// Send everything buffered for one run, in bounded chunks. Returns the
    /// server's cancel flag.
    fn flush_run(&self, run_id: i64, block: bool) -> Result<bool, String> {
        let mut cancel = false;
        loop {
            let (more, c) = self.flush_chunk(run_id, block)?;
            cancel |= c;
            if !more {
                return Ok(cancel);
            }
        }
    }

    /// Send one chunk. Returns (more pending, cancel flag).
    fn flush_chunk(&self, run_id: i64, block: bool) -> Result<(bool, bool), String> {
        let events = {
            let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
            let Some(buf) = runs.get_mut(&run_id) else {
                return Ok((false, false));
            };
            buf.seal_logs();
            if buf.pending.is_empty() {
                return Ok((false, buf.cancel));
            }
            buf.next_chunk()
        };
        let body = json!({"engine_id": self.engine_id, "run_id": run_id, "events": events});
        let result = if block {
            self.post_retry("/api/engine/report", &body, MAX_RETRY)
        } else {
            self.post_json(&self.agent, "/api/engine/report", &body)
        };
        match result {
            Ok((status, value)) if status < 300 => {
                let last_seq = value.get("last_seq").and_then(|v| v.as_i64()).unwrap_or(0);
                let cancel = value
                    .get("cancel")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
                let mut more = false;
                if let Some(buf) = runs.get_mut(&run_id) {
                    // Anything the server did not acknowledge stays for retry.
                    buf.pending.retain(|e| e.seq() > last_seq);
                    buf.last_contact = Some(Instant::now());
                    buf.cancel = buf.cancel || cancel;
                    more = !buf.pending.is_empty() || !buf.logs.is_empty();
                }
                Ok((more, cancel))
            }
            Ok((404, _)) => {
                // Run is gone (deleted): drop its buffer.
                let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(buf) = runs.get_mut(&run_id) {
                    buf.pending.clear();
                    buf.logs.clear();
                    buf.cancel = true;
                }
                Ok((false, true))
            }
            Ok((413, _)) => {
                // Still too large: shed the oldest log lines and report it.
                let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(buf) = runs.get_mut(&run_id) {
                    if let Some(ReportEvent::Logs { logs, .. }) = buf.pending.first_mut() {
                        let keep = logs.len() / 2;
                        buf.dropped_logs += logs.len() - keep;
                        logs.truncate(keep);
                    }
                }
                Err("report rejected as too large; dropped oldest log lines".into())
            }
            Ok((status, value)) => Err(format!("report rejected with {status}: {value}")),
            Err(e) => Err(e),
        }
    }

    fn heartbeat(&self, run_id: i64) -> Result<bool, String> {
        let body = json!({"engine_id": self.engine_id, "run_id": run_id});
        let (status, value) = self.post_json(&self.agent, "/api/engine/heartbeat", &body)?;
        if status >= 300 {
            return Err(format!("heartbeat status {status}"));
        }
        let cancel = value
            .get("cancel")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut runs = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(buf) = runs.get_mut(&run_id) {
            buf.last_contact = Some(Instant::now());
            buf.cancel = buf.cancel || cancel;
        }
        Ok(cancel)
    }
}

fn flusher(inner: Arc<Inner>) {
    let mut backoff = Duration::from_millis(0);
    while !inner.stop.load(Ordering::SeqCst) {
        std::thread::sleep(FLUSH_INTERVAL + backoff);
        let run_ids: Vec<i64> = inner
            .runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .copied()
            .collect();
        let mut failed = false;
        for run_id in run_ids {
            let (has_pending, needs_heartbeat) = {
                let mut runs = inner.runs.lock().unwrap_or_else(|p| p.into_inner());
                match runs.get_mut(&run_id) {
                    Some(buf) => {
                        buf.seal_logs();
                        (
                            !buf.pending.is_empty(),
                            buf.last_contact
                                .map(|t| t.elapsed() >= HEARTBEAT_INTERVAL)
                                .unwrap_or(true),
                        )
                    }
                    None => (false, false),
                }
            };
            if has_pending {
                if let Err(e) = inner.flush_run(run_id, false) {
                    inner.log_failure(&e);
                    failed = true;
                }
            } else if needs_heartbeat {
                if let Err(e) = inner.heartbeat(run_id) {
                    inner.log_failure(&e);
                    failed = true;
                }
            }
        }
        backoff = if failed {
            (backoff * 2 + Duration::from_millis(200)).min(Duration::from_secs(5))
        } else {
            Duration::ZERO
        };
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

/// HTTP client for engine children: work requests, run transitions, batched
/// task reports and logs, heartbeats.
#[pyclass(frozen)]
pub struct Client {
    inner: Arc<Inner>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

#[pymethods]
#[allow(clippy::too_many_arguments)]
impl Client {
    #[new]
    #[pyo3(signature = (base_url, engine_id, token=None))]
    fn new(base_url: String, engine_id: String, token: Option<String>) -> PyResult<Client> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let long_agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(LONG_POLL_TIMEOUT))
            .build()
            .into();
        let inner = Arc::new(Inner {
            base: base_url,
            engine_id,
            token,
            agent,
            long_agent,
            runs: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            last_failure_log: Mutex::new(None),
        });
        let thread_inner = inner.clone();
        let handle = std::thread::Builder::new()
            .name("cereyan-reporter".into())
            .spawn(move || flusher(thread_inner))
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(Client {
            inner,
            thread: Mutex::new(Some(handle)),
        })
    }

    #[getter]
    fn base_url(&self) -> String {
        self.inner.base.clone()
    }

    /// GET a JSON endpoint; returns (status, json text).
    fn get(&self, py: Python<'_>, path: String) -> PyResult<(u16, String)> {
        let inner = self.inner.clone();
        py.detach(move || inner.get_json(&path))
            .map(|(s, v)| (s, v.to_string()))
            .map_err(PyRuntimeError::new_err)
    }

    /// POST JSON; returns (status, json text). No retry.
    fn post(&self, py: Python<'_>, path: String, body: String) -> PyResult<(u16, String)> {
        let inner = self.inner.clone();
        let value: Value =
            serde_json::from_str(&body).map_err(|e| PyValueError::new_err(e.to_string()))?;
        py.detach(move || inner.post_json(&inner.agent, &path, &value))
            .map(|(s, v)| (s, v.to_string()))
            .map_err(PyRuntimeError::new_err)
    }

    /// Long-poll for work. Returns the JSON response text; on prolonged
    /// server absence returns {"exit": true}.
    #[pyo3(signature = (pid, source_dir, module, isolated=false, wait_ms=30000, nice=0))]
    fn get_work(
        &self,
        py: Python<'_>,
        pid: u32,
        source_dir: String,
        module: String,
        isolated: bool,
        wait_ms: u64,
        nice: u8,
    ) -> PyResult<String> {
        let inner = self.inner.clone();
        py.detach(move || {
            let body = json!({
                "engine_id": inner.engine_id, "pid": pid, "source_dir": source_dir,
                "module": module, "isolated": isolated, "wait_ms": wait_ms, "nice": nice,
            });
            let start = Instant::now();
            let mut delay = Duration::from_millis(200);
            loop {
                match inner.post_json(&inner.long_agent, "/api/engine/work", &body) {
                    Ok((status, value)) if status < 300 => return Ok(value.to_string()),
                    Ok((status, value)) => {
                        return Err(PyRuntimeError::new_err(format!(
                            "work request failed with {status}: {value}"
                        )))
                    }
                    Err(e) => {
                        if inner.stop.load(Ordering::SeqCst) || start.elapsed() > IDLE_RETRY {
                            return Ok(json!({"exit": true}).to_string());
                        }
                        inner.log_failure(&e);
                        std::thread::sleep(delay);
                        delay = (delay * 2).min(Duration::from_secs(5));
                    }
                }
            }
        })
    }

    fn begin_run(&self, run_id: i64) {
        let mut runs = self.inner.runs.lock().unwrap_or_else(|p| p.into_inner());
        runs.entry(run_id).or_default();
    }

    /// Flush and forget a run's buffer.
    fn end_run(&self, py: Python<'_>, run_id: i64) -> PyResult<()> {
        let inner = self.inner.clone();
        py.detach(move || {
            let _ = inner.flush_run(run_id, true);
            inner
                .runs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&run_id);
        });
        Ok(())
    }

    /// Propose a run state through the server. Flushes buffered events first.
    /// Returns (accepted, json) where json is the run on success or the
    /// rejection body (with `current`) on 409.
    #[pyo3(signature = (run_id, state_type, name=None, message=None, details=None, force=false))]
    fn transition_run(
        &self,
        py: Python<'_>,
        run_id: i64,
        state_type: &str,
        name: Option<&str>,
        message: Option<String>,
        details: Option<&str>,
        force: bool,
    ) -> PyResult<(bool, String)> {
        let state = parse_state(state_type, name, message, details)?;
        let inner = self.inner.clone();
        py.detach(move || {
            let _ = inner.flush_run(run_id, true);
            let body = json!({
                "type": state.state_type, "name": state.name, "message": state.message,
                "details": state.details, "force": force,
            });
            match inner.post_retry(&format!("/api/runs/{run_id}/transition"), &body, MAX_RETRY) {
                Ok((status, value)) if status < 300 => Ok((true, value.to_string())),
                Ok((409, value)) => Ok((false, value.to_string())),
                Ok((status, value)) => Err(PyRuntimeError::new_err(format!(
                    "transition failed with {status}: {value}"
                ))),
                Err(e) => Err(PyRuntimeError::new_err(e)),
            }
        })
    }

    #[pyo3(signature = (run_id, external_id, name, task_key, dynamic_key, parents=None))]
    fn task_run_created(
        &self,
        run_id: i64,
        external_id: &str,
        name: String,
        task_key: String,
        dynamic_key: String,
        parents: Option<Vec<String>>,
    ) -> PyResult<()> {
        let ext = Id::parse(external_id).ok_or_else(|| PyValueError::new_err("bad external id"))?;
        let parents: Vec<Id> = parents
            .unwrap_or_default()
            .iter()
            .filter_map(|p| Id::parse(p))
            .collect();
        let mut runs = self.inner.runs.lock().unwrap_or_else(|p| p.into_inner());
        let buf = runs.entry(run_id).or_default();
        buf.seal_logs();
        buf.next_seq += 1;
        buf.pending.push(ReportEvent::TaskRunCreated {
            seq: buf.next_seq,
            external_id: ext,
            name,
            task_key,
            dynamic_key,
            parents,
        });
        Ok(())
    }

    #[pyo3(signature = (run_id, external_id, state_type, name=None, message=None, details=None, force=false))]
    fn task_run_transition(
        &self,
        run_id: i64,
        external_id: &str,
        state_type: &str,
        name: Option<&str>,
        message: Option<String>,
        details: Option<&str>,
        force: bool,
    ) -> PyResult<String> {
        let ext = Id::parse(external_id).ok_or_else(|| PyValueError::new_err("bad external id"))?;
        let state = parse_state(state_type, name, message, details)?;
        let text = serde_json::to_string(&state).unwrap_or_default();
        let mut runs = self.inner.runs.lock().unwrap_or_else(|p| p.into_inner());
        let buf = runs.entry(run_id).or_default();
        buf.seal_logs();
        buf.next_seq += 1;
        buf.pending.push(ReportEvent::TaskRunTransition {
            seq: buf.next_seq,
            external_id: ext,
            state,
            force,
        });
        Ok(text)
    }

    /// Buffer a custom event for the run.
    #[pyo3(signature = (run_id, name, payload="{}", task_run_external_id=None))]
    fn emit_event(
        &self,
        run_id: i64,
        name: String,
        payload: &str,
        task_run_external_id: Option<&str>,
    ) -> PyResult<()> {
        let payload: Value = serde_json::from_str(payload)
            .map_err(|e| PyValueError::new_err(format!("payload is not JSON: {e}")))?;
        let ext = task_run_external_id.and_then(Id::parse);
        let mut runs = self.inner.runs.lock().unwrap_or_else(|p| p.into_inner());
        let buf = runs.entry(run_id).or_default();
        buf.seal_logs();
        buf.next_seq += 1;
        buf.pending.push(ReportEvent::Custom {
            seq: buf.next_seq,
            name,
            payload,
            task_run_external_id: ext,
        });
        Ok(())
    }

    /// Buffer an artifact (JSON data) for the run; returns its external id.
    #[pyo3(signature = (run_id, kind, data, key=None, task_run_external_id=None, external_id=None))]
    fn artifact(
        &self,
        run_id: i64,
        kind: String,
        data: &str,
        key: Option<String>,
        task_run_external_id: Option<&str>,
        external_id: Option<&str>,
    ) -> PyResult<String> {
        let data: Value = serde_json::from_str(data)
            .map_err(|e| PyValueError::new_err(format!("data is not JSON: {e}")))?;
        let ext = external_id
            .and_then(Id::parse)
            .unwrap_or_else(cereyan_core::new_id);
        let task_ext = task_run_external_id.and_then(Id::parse);
        let mut runs = self.inner.runs.lock().unwrap_or_else(|p| p.into_inner());
        let buf = runs.entry(run_id).or_default();
        buf.seal_logs();
        buf.next_seq += 1;
        buf.pending.push(ReportEvent::Artifact {
            seq: buf.next_seq,
            external_id: ext,
            task_run_external_id: task_ext,
            artifact_kind: kind,
            key,
            data,
        });
        Ok(ext.to_string())
    }

    /// Buffer one log record. Never blocks on the network.
    #[pyo3(signature = (run_id, task_run_external_id, level, logger, timestamp, message))]
    fn log(
        &self,
        run_id: i64,
        task_run_external_id: Option<&str>,
        level: i32,
        logger: String,
        timestamp: i64,
        message: String,
    ) {
        let ext = task_run_external_id.and_then(Id::parse);
        let mut runs = self.inner.runs.lock().unwrap_or_else(|p| p.into_inner());
        let buf = runs.entry(run_id).or_default();
        let buffered: usize = buf.logs.len() + buf.pending.len();
        if buffered >= MAX_BUFFERED_LOGS {
            buf.dropped_logs += 1;
            return;
        }
        buf.logs.push(NewLog {
            run_id,
            task_run_id: None,
            task_run_external_id: ext,
            level,
            logger,
            timestamp,
            message,
        });
    }

    /// Send everything buffered for a run now, waiting through outages.
    fn flush(&self, py: Python<'_>, run_id: i64) -> PyResult<bool> {
        let inner = self.inner.clone();
        py.detach(move || inner.flush_run(run_id, true))
            .map_err(PyRuntimeError::new_err)
    }

    fn cancel_requested(&self, run_id: i64) -> bool {
        self.inner
            .runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&run_id)
            .map(|b| b.cancel)
            .unwrap_or(false)
    }

    fn heartbeat(&self, py: Python<'_>, run_id: i64) -> PyResult<bool> {
        let inner = self.inner.clone();
        py.detach(move || inner.heartbeat(run_id))
            .map_err(PyRuntimeError::new_err)
    }

    fn dropped_logs(&self, run_id: i64) -> usize {
        self.inner
            .runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&run_id)
            .map(|b| b.dropped_logs)
            .unwrap_or(0)
    }

    #[pyo3(signature = (source_dir, module, isolated, traceback, nice=0))]
    fn report_failed(
        &self,
        py: Python<'_>,
        source_dir: String,
        module: String,
        isolated: bool,
        traceback: String,
        nice: u8,
    ) -> PyResult<()> {
        let inner = self.inner.clone();
        py.detach(move || {
            let body = json!({
                "engine_id": inner.engine_id, "source_dir": source_dir, "module": module,
                "isolated": isolated, "traceback": traceback, "nice": nice,
            });
            inner
                .post_retry("/api/engine/failed", &body, Duration::from_secs(30))
                .map(|_| ())
        })
        .map_err(PyRuntimeError::new_err)
    }

    fn close(&self, py: Python<'_>) {
        self.inner.stop.store(true, Ordering::SeqCst);
        let handle = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take();
        py.detach(move || {
            if let Some(h) = handle {
                let _ = h.join();
            }
        });
    }
}
