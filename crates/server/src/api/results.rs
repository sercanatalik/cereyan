//! Persisted results, cache entries and checkpoints for engines that are not
//! on the server's machine. Files stay under `<home>/storage` with the keys the
//! engine would have used locally, so every host sees the same entries and
//! retention treats them as before. Uploads come in chunks so a proxy with a
//! small body limit in front of the server does not refuse a large result.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::error::{ApiError, ApiResult};
use crate::state::AppState;

/// The most one result may take, whatever the flow's `checkpoint_max_bytes`.
const MAX_RESULT_BYTES: u64 = 256 * 1024 * 1024;

/// A key is one file name: letters, digits, `-`, `_`, `.`, and never a leading dot.
fn checked_key(key: &str) -> ApiResult<&str> {
    let ok = !key.is_empty()
        && key.len() <= 200
        && !key.starts_with('.')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(key)
    } else {
        Err(ApiError::BadRequest(format!("invalid result key {key:?}")))
    }
}

fn storage(state: &AppState) -> PathBuf {
    state.config.home.join("storage")
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct PartQuery {
    /// The chunk number, from 0. Part 0 starts the upload over.
    #[serde(default)]
    pub part: u32,
    /// Whether this is the last chunk: the result becomes visible when it lands.
    #[serde(default)]
    pub last: bool,
}

#[utoipa::path(put, path = "/api/results/{key}", params(("key" = String, Path), PartQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 204), (status = 400), (status = 413, description = "Over the size bound")))]
pub async fn put(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<PartQuery>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let key = checked_key(&key)?.to_string();
    let dir = storage(&state);
    tokio::task::spawn_blocking(move || -> ApiResult<StatusCode> {
        std::fs::create_dir_all(&dir).map_err(|e| ApiError::Internal(e.to_string()))?;
        let partial = dir.join(format!("{key}.upload"));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(q.part > 0)
            .truncate(q.part == 0)
            .open(&partial)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0) + body.len() as u64;
        if size > MAX_RESULT_BYTES {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Ok(StatusCode::PAYLOAD_TOO_LARGE);
        }
        file.write_all(&body)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        drop(file);
        if q.last {
            std::fs::rename(&partial, dir.join(&key))
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

#[utoipa::path(get, path = "/api/results/{key}", params(("key" = String, Path)),
    responses((status = 200, content_type = "application/octet-stream", body = Vec<u8>), (status = 404)))]
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Response> {
    let key = checked_key(&key)?.to_string();
    let path = storage(&state).join(key);
    let read = tokio::task::spawn_blocking(move || std::fs::read(&path))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match read {
        Ok(bytes) => {
            Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ApiError::NotFound("no such result".into()))
        }
        Err(e) => Err(ApiError::Internal(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::checked_key;

    #[test]
    fn keys_are_single_file_names() {
        assert!(checked_key("ckpt-01a0-01b2").is_ok());
        assert!(checked_key("e5d5f7c1.json").is_ok());
        for bad in ["", "../secret.key", "a/b", ".hidden", "a b"] {
            assert!(checked_key(bad).is_err(), "{bad} accepted");
        }
    }
}
