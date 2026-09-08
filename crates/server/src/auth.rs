//! Bearer-token authentication for the API. One static token from the
//! serve config; requests over the Unix socket are trusted by file mode.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::AppState;

/// Marker extension set by the Unix socket listener: skip the token check.
#[derive(Clone, Copy, Debug)]
pub struct TrustedTransport;

const COOKIE_NAME: &str = "cereyan_token";

/// Constant-time equality over the bytes of two strings.
fn equal(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        // Still touch every byte of the longer input.
        let mut acc = 0u8;
        for (x, y) in a.iter().zip(b.iter().cycle()) {
            acc |= x ^ y;
        }
        let _ = acc;
        return false;
    }
    let mut acc = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

fn cookie_token(req: &Request<Body>) -> Option<String> {
    let raw = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part
            .strip_prefix(COOKIE_NAME)
            .and_then(|r| r.strip_prefix('='))
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

fn bearer_token(req: &Request<Body>) -> Option<String> {
    let raw = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, value) = raw.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(value.trim().to_string())
    } else {
        None
    }
}

/// Middleware: require the configured token on `/api/*` except `/api/health`.
pub async fn require_token(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(expected) = state.config.token.as_deref() else {
        return next.run(req).await;
    };
    if req.uri().path() == "/api/health" || req.extensions().get::<TrustedTransport>().is_some() {
        return next.run(req).await;
    }
    let presented = bearer_token(&req).or_else(|| cookie_token(&req));
    match presented {
        Some(t) if equal(&t, expected) => next.run(req).await,
        Some(_) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "the API token was rejected"})),
        )
            .into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "this server requires an API token: send Authorization: Bearer <token> (CEREYAN_TOKEN or --token)"})),
        )
            .into_response(),
    }
}
