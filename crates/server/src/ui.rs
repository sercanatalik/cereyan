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
        Some(_) if candidate == "index.html" => index(&state),
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
        None => index(&state),
    }
}

/// The title the UI shows when `[ui] title` is missing or empty.
pub const DEFAULT_TITLE: &str = "cereyan";
const MAX_TITLE_CHARS: usize = 80;

/// The UI title in effect for a configured value: trimmed, and `cereyan` when
/// missing or empty. The error says why a value cannot be used.
pub fn normalize_title(raw: Option<&str>) -> Result<String, String> {
    let title = raw.unwrap_or("").trim();
    if title.is_empty() {
        return Ok(DEFAULT_TITLE.into());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "title must be at most {MAX_TITLE_CHARS} characters"
        ));
    }
    if title.chars().any(char::is_control) {
        return Err("title must not contain control characters".into());
    }
    Ok(title.to_string())
}

/// `index.html` with a `<base href>` naming the base path, so the relative
/// asset URLs resolve under it from any deep link and the UI can read it, and
/// with the UI title as its `<title>`, so the tab is right before the app loads.
fn index(state: &AppState) -> Response {
    match Assets::get("index.html") {
        Some(index) => {
            let html = String::from_utf8_lossy(&index.data);
            let html = with_title(&with_base(&html, &state.config.base_path), &state.title());
            (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                ],
                html.into_bytes(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "UI not built").into_response(),
    }
}

/// Insert `<base href="{base_path}/">` right after `<head>`. The base path is
/// validated at startup to URL-safe segments, so it needs no escaping.
fn with_base(html: &str, base_path: &str) -> String {
    let tag = format!("<head>\n    <base href=\"{base_path}/\" />");
    html.replacen("<head>", &tag, 1)
}

/// Replace the text of the first `<title>` element with the escaped title.
fn with_title(html: &str, title: &str) -> String {
    const CLOSE: &str = "</title>";
    match (html.find("<title>"), html.find(CLOSE)) {
        (Some(start), Some(end)) if start < end => format!(
            "{}<title>{}{CLOSE}{}",
            &html[..start],
            escape_html(title),
            &html[end + CLOSE.len()..]
        ),
        _ => html.to_string(),
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::{normalize_title, with_base, with_title, DEFAULT_TITLE};

    #[test]
    fn base_tag_follows_head() {
        let html = "<html><head><title>x</title></head></html>";
        let out = with_base(html, "/cereyan");
        assert!(out.contains("<head>\n    <base href=\"/cereyan/\" /><title>"));
        let root = with_base(html, "");
        assert!(root.contains("<base href=\"/\" />"));
    }

    #[test]
    fn title_replaces_the_title_element() {
        let html = "<head><title>cereyan</title></head>";
        assert_eq!(
            with_title(html, "Data Platform"),
            "<head><title>Data Platform</title></head>"
        );
        assert_eq!(
            with_title(html, "A & <B> \"q\""),
            "<head><title>A &amp; &lt;B&gt; &quot;q&quot;</title></head>"
        );
        assert_eq!(with_title("<head></head>", "x"), "<head></head>");
    }

    #[test]
    fn title_normalises() {
        assert_eq!(normalize_title(None).unwrap(), DEFAULT_TITLE);
        assert_eq!(normalize_title(Some("   ")).unwrap(), DEFAULT_TITLE);
        assert_eq!(normalize_title(Some("  Ops  ")).unwrap(), "Ops");
        assert_eq!(
            normalize_title(Some(&"é".repeat(80)))
                .unwrap()
                .chars()
                .count(),
            80
        );
        assert!(normalize_title(Some(&"x".repeat(81))).is_err());
        assert!(normalize_title(Some("a\nb")).is_err());
    }
}
