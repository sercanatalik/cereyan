//! `cereyan._core.Server`: start and stop the axum server from Python.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cereyan_server::{
    DispatchRequest, DispatchResponse, RouteDispatcher, RuleDispatcher, ServeConfig,
};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyTuple};

use crate::Store;

/// Calls the Python dispatcher for custom routes. Runs on a blocking thread
/// and takes the GIL for the duration of the handler.
struct PyDispatcher(Py<PyAny>);

impl RouteDispatcher for PyDispatcher {
    fn dispatch(&self, request: DispatchRequest) -> DispatchResponse {
        Python::attach(|py| {
            let body = PyBytes::new(py, &request.body);
            let result = self.0.bind(py).call1((
                request.route_id,
                request.method,
                request.path,
                request.path_params,
                request.query,
                request.headers,
                body,
            ));
            match result {
                Ok(value) => match parse_response(py, &value) {
                    Ok(resp) => resp,
                    Err(e) => internal(format!("bad dispatcher response: {e}")),
                },
                Err(e) => {
                    e.print(py);
                    internal("handler failed".into())
                }
            }
        })
    }
}

fn internal(message: String) -> DispatchResponse {
    DispatchResponse {
        status: 500,
        headers: vec![("content-type".into(), "application/json".into())],
        body: serde_json::json!({"error": message})
            .to_string()
            .into_bytes(),
    }
}

fn parse_response(_py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<DispatchResponse> {
    let tuple = value.cast::<PyTuple>()?;
    let status: u16 = tuple.get_item(0)?.extract()?;
    let headers: Vec<(String, String)> = tuple.get_item(1)?.extract()?;
    let body: Vec<u8> = tuple.get_item(2)?.extract()?;
    Ok(DispatchResponse {
        status,
        headers,
        body,
    })
}

/// Calls a Python callable for `call` actions of code rules.
struct PyRuleDispatcher(Py<PyAny>);

impl RuleDispatcher for PyRuleDispatcher {
    fn call(
        &self,
        callable: &str,
        event: &serde_json::Value,
        run: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Python::attach(|py| {
            let result = self
                .0
                .bind(py)
                .call1((callable, event.to_string(), run.to_string()));
            match result {
                Ok(value) => {
                    let text: String = value.extract().unwrap_or_else(|_| "null".into());
                    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
                }
                Err(e) => {
                    let msg = e.to_string();
                    e.print(py);
                    Err(msg)
                }
            }
        })
    }
}

#[pyclass(frozen)]
pub struct Server {
    inner: Mutex<Option<Arc<cereyan_server::Server>>>,
    port: u16,
    url: String,
}

#[pymethods]
impl Server {
    /// Start serving. `config` is the JSON form of ServeConfig; `dispatcher`
    /// is a callable handling custom routes.
    #[staticmethod]
    #[pyo3(signature = (store, config, dispatcher=None, rule_dispatcher=None))]
    fn start(
        py: Python<'_>,
        store: &Store,
        config: &str,
        dispatcher: Option<Py<PyAny>>,
        rule_dispatcher: Option<Py<PyAny>>,
    ) -> PyResult<Server> {
        let config: ServeConfig = serde_json::from_str(config)
            .map_err(|e| PyValueError::new_err(format!("invalid server config: {e}")))?;
        let store_arc = store.inner.clone();
        let dispatcher: Option<Arc<dyn RouteDispatcher>> =
            dispatcher.map(|d| Arc::new(PyDispatcher(d)) as Arc<dyn RouteDispatcher>);
        let rules: Option<Arc<dyn RuleDispatcher>> =
            rule_dispatcher.map(|d| Arc::new(PyRuleDispatcher(d)) as Arc<dyn RuleDispatcher>);
        let server = py
            .detach(move || {
                cereyan_server::Server::start_with(config, store_arc, dispatcher, rules)
            })
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let port = server.port();
        let url = server.url();
        Ok(Server {
            inner: Mutex::new(Some(Arc::new(server))),
            port,
            url,
        })
    }

    #[getter]
    fn port(&self) -> u16 {
        self.port
    }

    #[getter]
    fn url(&self) -> String {
        self.url.clone()
    }

    /// Wait up to `timeout` seconds; returns True once the server has stopped.
    fn wait(&self, py: Python<'_>, timeout: f64) -> bool {
        let server = self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .cloned();
        match server {
            None => true,
            Some(server) => {
                py.detach(move || server.wait(Duration::from_secs_f64(timeout.max(0.0))))
            }
        }
    }

    /// Graceful shutdown: stop accepting work, drain, checkpoint, remove
    /// the discovery file. Engines keep running.
    fn stop(&self, py: Python<'_>) {
        let server = self.inner.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(server) = server {
            py.detach(move || {
                server.stop();
                drop(server);
            });
        }
    }
}
