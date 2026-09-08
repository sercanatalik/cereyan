//! Embedded single-page UI served as the fallback route.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/../../ui/dist"]
struct Assets;

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let candidate = if path.is_empty() { "index.html" } else { path };
    match Assets::get(candidate) {
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
        None => match Assets::get("index.html") {
            Some(index) => (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
                index.data.into_owned(),
            )
                .into_response(),
            None => (StatusCode::NOT_FOUND, "UI not built").into_response(),
        },
    }
}
