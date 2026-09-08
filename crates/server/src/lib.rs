//! The cereyan server: axum API, SSE stream, in-memory working set, engine
//! supervisor, custom route bridge, and embedded UI, all on one tokio runtime
//! running on a dedicated thread.

pub mod api;
pub mod auth;
mod custom;
mod dispatch;
mod events;
mod index;
pub mod mcp;
mod process;
mod retention;
pub mod rules;
mod scheduler;
mod state;
mod stream;
mod supervisor;
pub mod timer;
mod ui;
mod validate;

pub use custom::{DispatchRequest, DispatchResponse, RouteDispatcher, RouteSpec};
pub use rules::RuleDispatcher;
pub use state::AppState;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cereyan_store::Store;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServeConfig {
    pub home: PathBuf,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub served_dir: Option<PathBuf>,
    /// Python interpreter used for engine children.
    #[serde(default = "default_python")]
    pub python: String,
    #[serde(default = "default_max_engines")]
    pub max_engines: usize,
    #[serde(default = "default_engine_max_runs")]
    pub engine_max_runs: u32,
    #[serde(default = "default_grace")]
    pub cancel_grace_secs: u64,
    #[serde(default = "default_heartbeat")]
    pub heartbeat_secs: u64,
    #[serde(default)]
    pub custom_routes: Vec<RouteSpec>,
    /// Flow ids registered from code by this server process.
    #[serde(default)]
    pub live_flows: Vec<i64>,
    #[serde(default)]
    pub version: String,
    /// Resource totals from cereyan.toml `[resources]`.
    #[serde(default)]
    pub resources: std::collections::HashMap<String, f64>,
    /// Default crash retry limit (flow decorators override it).
    #[serde(default = "default_crash_retries")]
    pub crash_retries_default: i64,
    /// Testing aid: rerun crashed runs after 200 ms instead of 5 to 15 s.
    #[serde(default)]
    pub fast_crash_rerun: bool,
    /// `[email]` from cereyan.toml.
    #[serde(default)]
    pub email: Option<EmailConfig>,
    /// Days to keep logs and events (default 30).
    #[serde(default = "default_retain_days")]
    pub retain_days: i64,
    /// Default catch-up policy name reported in settings.
    #[serde(default = "default_catchup")]
    pub catchup_default: String,
    /// API token; when set every `/api/*` route except health requires it.
    #[serde(default)]
    pub token: Option<String>,
    /// Unix socket path served next to the TCP listener (Unix only).
    #[serde(default)]
    pub socket: Option<PathBuf>,
    /// Testing aid: run the retention pass every few seconds instead of hourly.
    #[serde(default)]
    pub retention_interval_secs: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EmailConfig {
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    /// `none`, `starttls`, or `tls`.
    #[serde(default = "default_tls")]
    pub tls: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    pub from: String,
}

fn default_smtp_port() -> u16 {
    587
}
fn default_tls() -> String {
    "starttls".into()
}
fn default_retain_days() -> i64 {
    30
}
fn default_catchup() -> String {
    "skip".into()
}

fn default_crash_retries() -> i64 {
    5
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    4200
}
fn default_python() -> String {
    "python3".into()
}
fn default_max_engines() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}
fn default_engine_max_runs() -> u32 {
    100
}
fn default_grace() -> u64 {
    10
}
fn default_heartbeat() -> u64 {
    5
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("{0}")]
    Config(String),
    #[error("bind {addr}: {source}")]
    Bind {
        addr: String,
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] cereyan_store::StoreError),
}

/// A running server. Dropping it does not stop it; call `stop`.
pub struct Server {
    addr: SocketAddr,
    state: Arc<AppState>,
    shutdown: watch::Sender<bool>,
    done: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    finished: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl Server {
    /// Bind, reconcile, and start serving on a dedicated runtime thread.
    pub fn start(
        config: ServeConfig,
        store: Arc<Store>,
        dispatcher: Option<Arc<dyn RouteDispatcher>>,
    ) -> Result<Server, ServerError> {
        Server::start_with(config, store, dispatcher, None)
    }

    pub fn start_with(
        config: ServeConfig,
        store: Arc<Store>,
        dispatcher: Option<Arc<dyn RouteDispatcher>>,
        rule_dispatcher: Option<Arc<dyn RuleDispatcher>>,
    ) -> Result<Server, ServerError> {
        custom::check_conflicts(&config.custom_routes)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("cereyan-server")
            .enable_all()
            .build()?;
        let addr_text = format!("{}:{}", config.host, config.port);
        let listener = runtime.block_on(async {
            tokio::net::TcpListener::bind(&addr_text)
                .await
                .map_err(|e| ServerError::Bind {
                    addr: addr_text.clone(),
                    source: e,
                })
        })?;
        let addr = listener.local_addr()?;
        if !addr.ip().is_loopback() && config.token.is_none() {
            eprintln!(
                "warning: cereyan is listening on {addr}; the API is unauthenticated and reachable from the network"
            );
        }
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let state = Arc::new(AppState::new(
            config,
            store,
            dispatcher,
            addr,
            shutdown_rx.clone(),
        )?);
        state.set_self();
        *state
            .rule_dispatcher
            .write()
            .unwrap_or_else(|e| e.into_inner()) = rule_dispatcher;
        rules::load(&state);
        if let Ok(Some(v)) = state.store.kv_get("settings.retain_days") {
            if let Ok(d) = v.parse::<i64>() {
                state
                    .retain_days
                    .store(d, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if let Ok(Some(v)) = state.store.kv_get("settings.crash_retries") {
            if let Ok(c) = v.parse::<i64>() {
                state
                    .crash_retries_default
                    .store(c, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if let Ok(Some(saved)) = state.store.kv_get("settings.resources") {
            if let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, f64>>(&saved)
            {
                state.supervisor.set_totals(&map);
            }
        }
        state.reconcile()?;
        // Code-declared schedules of live flows.
        if let Ok(flows) = state.store.list_flows(None) {
            for flow in flows.iter().filter(|f| state.is_live(f.id)) {
                let options = cereyan_core::FlowOptions::from_map(&flow.options);
                if let Err(e) = scheduler::sync_code_schedules(&state, flow, &options.schedules) {
                    let _ = state
                        .store
                        .set_flow_error(flow.id, Some(format!("schedule: {e}")));
                }
            }
        }
        scheduler::start(&state);
        rules::start(&state);
        events::emit_registered_flows(&state);
        state.write_discovery_file()?;

        let router = api::router(state.clone());
        // Optional Unix socket: same routes, trusted by file mode (no token).
        let socket_listener = match state.config.socket.clone() {
            Some(path) => Some(runtime.block_on(bind_unix_socket(&path))?),
            None => None,
        };
        if let Some(path) = &state.config.socket {
            eprintln!(
                "cereyan: also listening on unix socket {} (clients there are trusted by file permission)",
                path.display()
            );
        }
        let finished = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let finished_inner = finished.clone();
        let state_inner = state.clone();
        let handle = std::thread::Builder::new()
            .name("cereyan-runtime".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let monitor = tokio::spawn(supervisor::monitor_loop(
                        state_inner.clone(),
                        shutdown_rx.clone(),
                    ));
                    let sched = tokio::spawn(scheduler::run_loop(
                        state_inner.clone(),
                        shutdown_rx.clone(),
                    ));
                    let retention = tokio::spawn(retention::run_loop(
                        state_inner.clone(),
                        shutdown_rx.clone(),
                    ));
                    let mut rx = shutdown_rx.clone();
                    // Unix only: `bind_unix_socket` refuses on other platforms, so
                    // `socket_listener` is always None there and this serve path would
                    // not typecheck against its `()` placeholder listener.
                    #[cfg(unix)]
                    let socket_task = socket_listener.map(|unix| {
                        let trusted = router
                            .clone()
                            .layer(axum::Extension(auth::TrustedTransport));
                        let mut rx = shutdown_rx.clone();
                        tokio::spawn(async move {
                            let serve =
                                axum::serve(unix, trusted).with_graceful_shutdown(async move {
                                    let _ = rx.wait_for(|v| *v).await;
                                });
                            if let Err(e) = serve.await {
                                eprintln!("cereyan socket server error: {e}");
                            }
                        })
                    });
                    #[cfg(not(unix))]
                    let socket_task: Option<tokio::task::JoinHandle<()>> = {
                        let _ = &socket_listener;
                        None
                    };
                    let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
                        let _ = rx.wait_for(|v| *v).await;
                    });
                    if let Err(e) = serve.await {
                        eprintln!("cereyan server error: {e}");
                    }
                    if let Some(task) = socket_task {
                        let _ = task.await;
                    }
                    monitor.abort();
                    sched.abort();
                    retention.abort();
                    state_inner.shutdown_cleanup();
                });
                // Drop the runtime with a bounded wait so a stuck task cannot
                // hang shutdown.
                runtime.shutdown_timeout(Duration::from_secs(2));
                let (lock, cv) = &*finished_inner;
                *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
                cv.notify_all();
            })?;
        Ok(Server {
            addr,
            state,
            shutdown: shutdown_tx,
            done: std::sync::Mutex::new(Some(handle)),
            finished,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Bind the Unix socket: replace a stale file nothing accepts on, refuse a
/// live one, and restrict the new file to the owner.
#[cfg(unix)]
async fn bind_unix_socket(path: &std::path::Path) -> Result<tokio::net::UnixListener, ServerError> {
    if path.exists() {
        match tokio::net::UnixStream::connect(path).await {
            Ok(_) => {
                return Err(ServerError::Bind {
                    addr: path.display().to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "another server accepts connections on this socket",
                    ),
                });
            }
            Err(_) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = tokio::net::UnixListener::bind(path).map_err(|e| ServerError::Bind {
        addr: path.display().to_string(),
        source: e,
    })?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

#[cfg(not(unix))]
async fn bind_unix_socket(path: &std::path::Path) -> Result<(), ServerError> {
    Err(ServerError::Bind {
        addr: path.display().to_string(),
        source: std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Unix sockets are not supported on this platform",
        ),
    })
}

impl Server {
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    /// Request a graceful shutdown and wait for it to complete.
    pub fn stop(&self) {
        self.state.begin_shutdown();
        let _ = self.shutdown.send(true);
        if let Some(handle) = self.done.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }

    /// Wait up to `timeout` for the server to finish. Returns true once it has.
    pub fn wait(&self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.finished;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if *guard {
            return true;
        }
        let (guard, _) = cv
            .wait_timeout(guard, timeout)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}
