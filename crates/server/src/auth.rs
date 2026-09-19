//! Authentication for the API: one static token from the serve config, and an
//! optional authenticator hook that validates any other credential. Requests
//! over the Unix socket are trusted by file mode.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::AppState;
use crate::{ServeConfig, ServerError};

/// Marker extension set by the Unix socket listener: skip the check.
#[derive(Clone, Copy, Debug)]
pub struct TrustedTransport;

/// The user the authenticator named for this request.
#[derive(Clone, Debug)]
pub struct AuthenticatedUser(pub String);

/// Validates a credential the static token did not match. Called on a
/// blocking thread; implementations take the GIL themselves.
pub trait Authenticator: Send + Sync {
    /// The user's name, or `None` to reject the credential.
    fn authenticate(&self, credential: &str) -> Option<String>;
}

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

fn cookie_value(req: &Request<Body>, name: &str) -> Option<String> {
    let raw = req.headers().get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        part.trim()
            .strip_prefix(name)
            .and_then(|r| r.strip_prefix('='))
            .map(|v| v.trim().to_string())
    })
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

/// Whether a path needs a credential. With scope `api`: `/mcp` and everything
/// under `/api/` except `/api/health`, so custom routes outside `/api/` and the
/// UI stay open. With scope `all`: every path except `/api/health`.
fn protected(path: &str, all: bool) -> bool {
    if path == "/api/health" {
        return false;
    }
    all || path == "/mcp" || path.starts_with("/api/")
}

/// `created_by` for a run a request creates: the signed-in user, else `fallback`.
pub fn run_creator(user: Option<&AuthenticatedUser>, fallback: &str) -> String {
    match user {
        Some(user) => format!("user:{}", user.0),
        None => fallback.to_string(),
    }
}

/// A random token for engines when auth is enabled without a configured one.
pub(crate) fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where a server bound beyond loopback keeps the token it generated.
pub fn token_file_path(home: &Path) -> PathBuf {
    home.join("token")
}

/// The generated token of `home`, created on first use and reused after, and
/// whether this call wrote it. An empty or unreadable file is replaced, since
/// a token nobody can read protects nothing. The home's own permissions are
/// the guarantee, as for `secret.key`; the owner-only mode is a second layer.
pub fn load_or_create_token_file(home: &Path) -> std::io::Result<(String, bool)> {
    let path = token_file_path(home);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let existing = text.trim();
        if !existing.is_empty() {
            return Ok((existing.to_string(), false));
        }
    }
    let token = generate_token();
    std::fs::create_dir_all(home)?;
    std::fs::write(&path, format!("{token}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok((token, true))
}

/// Reject auth settings that cannot work; Python names the source first.
pub(crate) fn check(config: &ServeConfig, authenticator: bool) -> Result<(), ServerError> {
    let fail = |message: String| Err(ServerError::Config(message));
    match config.auth_scope.as_str() {
        "api" => {}
        "all" if authenticator => {}
        "all" => {
            return fail(
                "auth_scope = \"all\" requires enable_auth and a registered authenticator".into(),
            )
        }
        other => {
            return fail(format!(
                "invalid auth_scope {other:?}: expected \"api\" or \"all\""
            ))
        }
    }
    if let Some(name) = &config.auth_cookie {
        if name == COOKIE_NAME {
            return fail(format!(
                "auth_cookie cannot be {COOKIE_NAME:?}: that cookie holds the API token"
            ));
        }
        if !valid_cookie_name(name) {
            return fail(format!(
                "invalid auth_cookie {name:?}: expected a cookie name"
            ));
        }
    }
    if let Some(url) = &config.login_url {
        if !valid_login_url(url) {
            return fail(format!(
                "invalid login_url {url:?}: expected an http or https URL, or a path starting with /"
            ));
        }
    }
    Ok(())
}

fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn valid_login_url(url: &str) -> bool {
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    if let Some(rest) = url.strip_prefix('/') {
        // `//host` and `/\host` leave the origin in a browser.
        return !rest.starts_with('/') && !rest.starts_with('\\');
    }
    let lower = url.to_ascii_lowercase();
    ["http://", "https://"].iter().any(|scheme| {
        lower
            .strip_prefix(scheme)
            .and_then(|rest| rest.split(['/', '?', '#']).next())
            .is_some_and(|host| !host.is_empty())
    })
}

/// Middleware: require a credential on the paths `protected` names. The socket
/// is trusted, the static token is compared next so engines never reach the
/// authenticator, and the authenticator sees any other credential.
pub async fn require_token(
    State(state): State<Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let hook = state.authenticator.clone();
    let expected = state.config.token.as_deref();
    if expected.is_none() && hook.is_none() {
        return next.run(req).await;
    }
    if req.extensions().get::<TrustedTransport>().is_some()
        || !protected(req.uri().path(), state.config.auth_scope == "all")
    {
        return next.run(req).await;
    }
    let bearer = bearer_token(&req);
    let presented = bearer.clone().or_else(|| cookie_value(&req, COOKIE_NAME));
    if let (Some(expected), Some(token)) = (expected, presented.as_deref()) {
        if equal(token, expected) {
            return next.run(req).await;
        }
    }
    let Some(hook) = hook else {
        return reject(&state, &req, presented.is_some());
    };
    let credential = bearer
        .or_else(|| {
            let name = state.config.auth_cookie.as_deref()?;
            cookie_value(&req, name)
        })
        .filter(|c| !c.is_empty());
    let Some(credential) = credential else {
        return reject(&state, &req, false);
    };
    let user = tokio::task::spawn_blocking(move || hook.authenticate(&credential))
        .await
        .ok()
        .flatten()
        .filter(|u| !u.is_empty());
    match user {
        Some(user) => {
            req.extensions_mut().insert(AuthenticatedUser(user));
            next.run(req).await
        }
        None => reject(&state, &req, true),
    }
}

/// 401 naming the auth mode and the sign-in URL, or, for a browser opening a
/// page under scope `all`, a short page with a sign-in link.
fn reject(state: &AppState, req: &Request<Body>, sent: bool) -> Response {
    let hook = state.authenticator.is_some();
    let login_url = state.config.login_url.as_deref();
    if state.config.auth_scope == "all" && wants_html(req) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            sign_in_page(login_url),
        )
            .into_response();
    }
    let error = match (hook, sent) {
        (false, false) => "this server requires an API token: send Authorization: Bearer <token> (CEREYAN_TOKEN or --token)",
        (false, true) => "the API token was rejected",
        (true, false) => "not signed in: send a credential in the Authorization header or the sign-in cookie",
        (true, true) => "the credential was rejected",
    };
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": error,
            "auth": if hook { "hook" } else { "token" },
            "login_url": login_url,
        })),
    )
        .into_response()
}

fn wants_html(req: &Request<Body>) -> bool {
    req.headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

fn sign_in_page(login_url: Option<&str>) -> String {
    let link = login_url
        .map(|url| format!("<p><a href=\"{}\">Sign in</a></p>", escape_html(url)))
        .unwrap_or_default();
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Not signed in</title></head>\
         <body><h1>Not signed in</h1><p>Sign in with your identity provider, then reload this page.</p>{link}</body></html>"
    )
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(extra: serde_json::Value) -> ServeConfig {
        let mut value = json!({"home": "/tmp/cereyan-auth-test"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn request(cookie: &str) -> Request<Body> {
        Request::builder()
            .uri("/api/runs")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn cookies_are_found_by_name() {
        let req = request("other=1; SSO_SESSION=abc; cereyan_token=t");
        assert_eq!(cookie_value(&req, "SSO_SESSION").as_deref(), Some("abc"));
        assert_eq!(cookie_value(&req, "cereyan_token").as_deref(), Some("t"));
        assert_eq!(cookie_value(&req, "SSO"), None);
        assert_eq!(cookie_value(&req, "missing"), None);
    }

    #[test]
    fn scope_decides_protected_paths() {
        for (path, api, all) in [
            ("/api/health", false, false),
            ("/api/runs", true, true),
            ("/mcp", true, true),
            ("/", false, true),
            ("/assets/index.js", false, true),
            ("/webhook", false, true),
        ] {
            assert_eq!(protected(path, false), api, "{path} with api");
            assert_eq!(protected(path, true), all, "{path} with all");
        }
    }

    #[test]
    fn settings_are_checked() {
        assert!(check(&config(json!({})), false).is_ok());
        assert!(check(&config(json!({"auth_scope": "all"})), true).is_ok());
        assert!(check(&config(json!({"auth_scope": "all"})), false).is_err());
        assert!(check(&config(json!({"auth_scope": "everything"})), true).is_err());
        assert!(check(&config(json!({"auth_cookie": "SSO_SESSION"})), true).is_ok());
        assert!(check(&config(json!({"auth_cookie": "cereyan_token"})), true).is_err());
        assert!(check(&config(json!({"auth_cookie": "a b"})), true).is_err());
        assert!(check(&config(json!({"auth_cookie": ""})), true).is_err());
    }

    #[test]
    fn login_urls_must_stay_http_or_a_path() {
        for ok in [
            "https://sso.example.com/login",
            "http://sso:8080",
            "/login",
            "/",
        ] {
            assert!(valid_login_url(ok), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "//evil.example.com",
            "/\\evil.example.com",
            "https://",
            "ftp://x",
            "login",
            "https://a b",
        ] {
            assert!(!valid_login_url(bad), "{bad}");
        }
    }

    #[test]
    fn sign_in_page_escapes_the_link() {
        let page = sign_in_page(Some("https://sso/login?a=1&b=\"x\""));
        assert!(page.contains("href=\"https://sso/login?a=1&amp;b=&quot;x&quot;\""));
        assert!(!sign_in_page(None).contains("<a "));
    }

    #[test]
    fn generated_tokens_are_long_and_distinct() {
        let a = generate_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, generate_token());
    }

    #[test]
    fn run_creator_prefers_the_user() {
        let user = AuthenticatedUser("alice".into());
        assert_eq!(run_creator(Some(&user), "client"), "user:alice");
        assert_eq!(run_creator(None, "client"), "client");
    }

    #[test]
    fn token_file_is_created_reused_and_replaced_when_empty() {
        let home = tempfile::tempdir().unwrap();
        let (first, created) = load_or_create_token_file(home.path()).unwrap();
        assert!(created);
        assert_eq!(first.len(), 64);
        assert_eq!(
            std::fs::read_to_string(token_file_path(home.path())).unwrap(),
            format!("{first}\n")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_file_path(home.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let (second, created) = load_or_create_token_file(home.path()).unwrap();
        assert!(!created);
        assert_eq!(second, first);

        std::fs::write(token_file_path(home.path()), "  \n").unwrap();
        let (third, created) = load_or_create_token_file(home.path()).unwrap();
        assert!(created);
        assert_ne!(third, first);
    }
}
