//! Embedded single-page UI served as the fallback route.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

use crate::state::AppState;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/../../ui/dist"]
struct Assets;

pub async fn serve(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let candidate = if path.is_empty() { "index.html" } else { path };
    match Assets::get(candidate) {
        Some(_) if candidate == "index.html" => index(&state.config.base_path),
        Some(file) => {
            let mime = mime_guess::from_path(candidate).first_or_octet_stream();
            let cache = if candidate.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                ],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => index(&state.config.base_path),
    }
}

/// `index.html` with a `<base href>` naming the base path, so the relative
/// asset URLs resolve under it from any deep link and the UI can read it.
fn index(base_path: &str) -> Response {
    match Assets::get("index.html") {
        Some(index) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
                (header::CACHE_CONTROL, "no-cache".to_string()),
            ],
            with_base(&index.data, base_path),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "UI not built").into_response(),
    }
}

/// Insert `<base href="{base_path}/">` right after `<head>`. The base path is
/// validated at startup to URL-safe segments, so it needs no escaping.
fn with_base(html: &[u8], base_path: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(html);
    let tag = format!("<head>\n    <base href=\"{base_path}/\" />");
    text.replacen("<head>", &tag, 1).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::with_base;

    #[test]
    fn base_tag_follows_head() {
        let html = b"<html><head><title>x</title></head></html>";
        let out = String::from_utf8(with_base(html, "/cereyan")).unwrap();
        assert!(out.contains("<head>\n    <base href=\"/cereyan/\" /><title>"));
        let root = String::from_utf8(with_base(html, "")).unwrap();
        assert!(root.contains("<base href=\"/\" />"));
    }
}
