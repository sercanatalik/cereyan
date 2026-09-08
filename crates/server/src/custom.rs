//! Bridge between axum and user-defined Python route handlers.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{RawPathParams, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put, MethodRouter};
use axum::Router;
use serde::{Deserialize, Serialize};

use crate::state::AppState;
use crate::ServerError;

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RouteSpec {
    pub id: usize,
    pub method: String,
    pub path: String,
    /// Where the handler is defined (module and function), for the settings page.
    #[serde(default)]
    pub source: Option<String>,
}

pub struct DispatchRequest {
    pub route_id: usize,
    pub method: String,
    pub path: String,
    pub path_params: Vec<(String, String)>,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct DispatchResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Runs a user handler. Called on a blocking thread; implementations take
/// the GIL themselves.
pub trait RouteDispatcher: Send + Sync {
    fn dispatch(&self, request: DispatchRequest) -> DispatchResponse;
}

/// Built-in paths that custom routes may not shadow.
pub const BUILTIN_PATHS: &[&str] = &[
    "/api/health",
    "/api/server",
    "/api/flows",
    "/api/flows/{id}",
    "/api/flows/{id}/runs",
    "/api/runs",
    "/api/runs/{id}",
    "/api/runs/{id}/tasks",
    "/api/runs/{id}/logs",
    "/api/runs/{id}/transition",
    "/api/runs/{id}/cancel",
    "/api/task-runs",
    "/api/task-runs/{id}",
    "/api/task-runs/{id}/logs",
    "/api/counts",
    "/api/stream",
    "/api/openapi.json",
    "/api/engine/work",
    "/api/engine/report",
    "/api/engine/heartbeat",
    "/api/engine/failed",
    "/api/runs/{id}/graph",
    "/api/flows/{id}/schedules",
    "/api/flows/{id}/upcoming",
    "/api/flows/{id}/pause",
    "/api/flows/{id}/resume",
    "/api/flows/{id}/backfill",
    "/api/flows/{id}/backfills",
    "/api/schedules/preview",
    "/api/schedules/{sid}",
    "/api/schedules/{sid}/pause",
    "/api/schedules/{sid}/resume",
    "/api/backfills/{id}",
    "/api/backfills/{id}/prefilter",
    "/api/backfills/{id}/cancel",
    "/api/settings",
    "/api/events",
    "/api/resources/acquire",
    "/api/resources/release",
    "/api/rules",
    "/api/rules/{id}",
    "/api/rules/{id}/firings",
    "/api/rules/{id}/test",
    "/api/runs/{id}/artifacts",
    "/api/task-runs/{id}/artifacts",
    "/api/variables",
    "/api/variables/{name}",
    "/api/events/{id}",
    "/api/artifacts",
];

fn normalize(path: &str) -> String {
    let mut out = String::new();
    for seg in path.trim_end_matches('/').split('/') {
        if seg.is_empty() {
            continue;
        }
        out.push('/');
        if seg.starts_with('{') && seg.ends_with('}') {
            out.push_str("{}");
        } else {
            out.push_str(seg);
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

pub fn check_conflicts(routes: &[RouteSpec]) -> Result<(), ServerError> {
    let builtins: Vec<String> = BUILTIN_PATHS.iter().map(|p| normalize(p)).collect();
    let mut seen: Vec<(String, String)> = Vec::new();
    for r in routes {
        let n = normalize(&r.path);
        if builtins.contains(&n) {
            return Err(ServerError::Config(format!(
                "custom route {} {} collides with the built-in cereyan route {}",
                r.method, r.path, r.path
            )));
        }
        if !r.path.starts_with('/') {
            return Err(ServerError::Config(format!(
                "custom route path {:?} must start with '/'",
                r.path
            )));
        }
        let key = (r.method.to_ascii_uppercase(), n.clone());
        if seen.contains(&key) {
            return Err(ServerError::Config(format!(
                "custom route {} {} is registered twice",
                r.method, r.path
            )));
        }
        seen.push(key);
    }
    Ok(())
}

pub fn attach(router: Router<Arc<AppState>>, routes: &[RouteSpec]) -> Router<Arc<AppState>> {
    let mut router = router;
    // Group by path so several methods on one path share a MethodRouter.
    let mut paths: Vec<String> = Vec::new();
    for r in routes {
        if !paths.contains(&r.path) {
            paths.push(r.path.clone());
        }
    }
    for path in paths {
        let mut method_router: MethodRouter<Arc<AppState>> = MethodRouter::new();
        for r in routes.iter().filter(|r| r.path == path) {
            let id = r.id;
            let handler = move |State(state): State<Arc<AppState>>,
                                method: Method,
                                params: RawPathParams,
                                RawQuery(query): RawQuery,
                                headers: HeaderMap,
                                body: Bytes| async move {
                dispatch(state, id, method, params, query, headers, body).await
            };
            method_router = match r.method.to_ascii_uppercase().as_str() {
                "GET" => method_router.merge(get(handler)),
                "POST" => method_router.merge(post(handler)),
                "PUT" => method_router.merge(put(handler)),
                "DELETE" => method_router.merge(delete(handler)),
                "PATCH" => method_router.merge(axum::routing::patch(handler)),
                _ => method_router,
            };
        }
        router = router.route(&path, method_router);
    }
    router
}

async fn dispatch(
    state: Arc<AppState>,
    route_id: usize,
    method: Method,
    params: RawPathParams,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(dispatcher) = state.dispatcher.clone() else {
        return (StatusCode::NOT_IMPLEMENTED, "no route dispatcher").into_response();
    };
    let path = state
        .config
        .custom_routes
        .iter()
        .find(|r| r.id == route_id)
        .map(|r| r.path.clone())
        .unwrap_or_default();
    let request = DispatchRequest {
        route_id,
        method: method.to_string(),
        path,
        path_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        query: query.unwrap_or_default(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect(),
        body: body.to_vec(),
    };
    let result = tokio::task::spawn_blocking(move || dispatcher.dispatch(request)).await;
    match result {
        Ok(resp) => {
            let mut response = Response::new(axum::body::Body::from(resp.body));
            *response.status_mut() =
                StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            for (k, v) in resp.headers {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(&v),
                ) {
                    response.headers_mut().insert(name, value);
                }
            }
            response
        }
        Err(e) => {
            eprintln!("custom route handler panicked: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": "handler failed"})),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(method: &str, path: &str) -> RouteSpec {
        RouteSpec {
            id: 0,
            method: method.into(),
            path: path.into(),
            source: None,
        }
    }

    #[test]
    fn builtin_collisions_are_rejected() {
        assert!(check_conflicts(&[spec("GET", "/api/runs")]).is_err());
        assert!(check_conflicts(&[spec("GET", "/api/runs/{run}")]).is_err());
        assert!(check_conflicts(&[spec("POST", "/api/flows/{id}/runs/")]).is_err());
        assert!(check_conflicts(&[spec("GET", "/health"), spec("GET", "/api/ext/ping")]).is_ok());
        assert!(check_conflicts(&[spec("GET", "/x"), spec("GET", "/x")]).is_err());
        assert!(check_conflicts(&[spec("GET", "/x"), spec("POST", "/x")]).is_ok());
        assert!(check_conflicts(&[spec("GET", "relative")]).is_err());
    }
}
