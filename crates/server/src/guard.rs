//! Host and Origin checks on the TCP listener, ahead of the token check.
//!
//! A server without a token trusts whatever reaches its port, and the user's
//! browser reaches it on behalf of any page they open. The Host check stops DNS
//! rebinding: only names that cannot be pointed elsewhere (IP literals and
//! `localhost`), the configured host, and `allowed_hosts` are answered. The
//! Origin check stops cross-site requests: a browser marks a request a page
//! sends with `Origin`, which must be the server's own or a listed host.
//! Clients that are not browsers send no `Origin`. The Unix socket skips both.

use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::auth::TrustedTransport;
use crate::{ServeConfig, ServerError};

const HOW_TO_ALLOW: &str =
    "add it to allowed_hosts (--allowed-host, CEREYAN_ALLOWED_HOSTS, or [server] allowed_hosts)";

/// The host names a server answers to beside IP literals and `localhost`.
#[derive(Clone, Debug, Default)]
pub struct Guard {
    /// `host` from the serve config, normalised.
    configured: String,
    /// `allowed_hosts`, normalised.
    allowed: Vec<String>,
}

impl Guard {
    pub fn new(config: &ServeConfig) -> Guard {
        Guard {
            configured: normalize(&config.host),
            allowed: config.allowed_hosts.iter().map(|h| normalize(h)).collect(),
        }
    }

    fn listed(&self, host: &str) -> bool {
        self.allowed.iter().any(|a| a == host)
    }

    /// Whether a request whose Host names `host` (normalised, no port) is answered.
    fn host_allowed(&self, host: &str) -> bool {
        host.parse::<IpAddr>().is_ok()
            || host == "localhost"
            || host == self.configured
            || self.listed(host)
    }

    /// Whether a request carrying `origin` is accepted: the origin is the
    /// request's own, or its host is listed. A Host without a port matches any
    /// origin port, since behind a proxy the server cannot know the public one.
    fn origin_allowed(&self, origin: &str, host_header: Option<&str>) -> bool {
        let Some((scheme, authority)) = origin.split_once("://") else {
            return false;
        };
        let default_port = match scheme.to_ascii_lowercase().as_str() {
            "http" => 80,
            "https" => 443,
            _ => return false,
        };
        if authority.contains(['/', '?', '#', '@']) {
            return false;
        }
        let Some((host, port)) = split_authority(authority) else {
            return false;
        };
        if self.listed(&host) {
            return true;
        }
        let Some((own, own_port)) = host_header.and_then(split_authority) else {
            return false;
        };
        host == own && own_port.is_none_or(|p| p == port.unwrap_or(default_port))
    }
}

/// Host and port of `host[:port]`, with the host normalised. `None` when the
/// host is empty or the port is not a number.
fn split_authority(authority: &str) -> Option<(String, Option<u16>)> {
    let authority = authority.trim();
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':')?),
        };
        (host, port)
    } else if authority.matches(':').count() > 1 {
        // An IPv6 address without brackets carries no port.
        (authority, None)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok()?),
        None => None,
    };
    Some((host.to_ascii_lowercase(), port))
}

/// Lowercase, trimmed, and an IPv6 address without its brackets.
fn normalize(host: &str) -> String {
    let host = host.trim();
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_ascii_lowercase()
}

/// Reject `allowed_hosts` entries that are not a host name or an IP address;
/// Python names the source first.
pub(crate) fn check_config(config: &ServeConfig) -> Result<(), ServerError> {
    match config.allowed_hosts.iter().find(|e| !valid_entry(e)) {
        Some(entry) => Err(ServerError::Config(format!(
            "invalid allowed_hosts entry {entry:?}: expected a host name or an IP address, without a scheme, port, or path"
        ))),
        None => Ok(()),
    }
}

fn valid_entry(entry: &str) -> bool {
    let host = normalize(entry);
    host.parse::<IpAddr>().is_ok()
        || host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Middleware: refuse a request for a host this server does not answer to,
/// and a request a browser sent from another site's page.
pub async fn check(State(guard): State<Arc<Guard>>, req: Request<Body>, next: Next) -> Response {
    if req.extensions().get::<TrustedTransport>().is_some() {
        return next.run(req).await;
    }
    let host = match req.headers().get(header::HOST) {
        Some(value) => match value.to_str() {
            Ok(v) => Some(v.to_string()),
            Err(_) => return refuse_host(""),
        },
        None => req.uri().authority().map(|a| a.as_str().to_string()),
    };
    if let Some(raw) = &host {
        match split_authority(raw) {
            Some((name, _)) if guard.host_allowed(&name) => {}
            Some((name, _)) => return refuse_host(&name),
            None => return refuse_host(raw),
        }
    }
    if let Some(value) = req.headers().get(header::ORIGIN) {
        let origin = value.to_str().unwrap_or("");
        if !guard.origin_allowed(origin, host.as_deref()) {
            return refuse_origin(origin);
        }
    }
    next.run(req).await
}

fn refuse_host(host: &str) -> Response {
    let error = format!(
        "this server does not answer to host {host:?}: if you reach it under that name, {HOW_TO_ALLOW}"
    );
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": error, "host": host})),
    )
        .into_response()
}

fn refuse_origin(origin: &str) -> Response {
    let error = format!(
        "requests from pages at {origin:?} are refused: if you trust pages on that host, {HOW_TO_ALLOW}"
    );
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": error, "origin": origin})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Method;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn guard(host: &str, allowed: &[&str]) -> Guard {
        let config: ServeConfig = serde_json::from_value(json!({
            "home": "/tmp/cereyan-guard-test",
            "host": host,
            "allowed_hosts": allowed,
        }))
        .unwrap();
        Guard::new(&config)
    }

    #[test]
    fn authorities_split_into_host_and_port() {
        for (raw, host, port) in [
            ("127.0.0.1:4200", "127.0.0.1", Some(4200)),
            ("LocalHost", "localhost", None),
            ("[::1]:4200", "::1", Some(4200)),
            ("[::1]", "::1", None),
            ("::1", "::1", None),
            ("cereyan.example.com:443", "cereyan.example.com", Some(443)),
        ] {
            assert_eq!(
                split_authority(raw),
                Some((host.to_string(), port)),
                "{raw}"
            );
        }
        for bad in ["", ":4200", "host:port", "[::1", "[::1]4200", "a:99999"] {
            assert_eq!(split_authority(bad), None, "{bad}");
        }
    }

    #[test]
    fn hosts_that_cannot_be_rebound_are_answered() {
        let g = guard("127.0.0.1", &[]);
        for ok in ["127.0.0.1", "0.0.0.0", "192.168.1.5", "::1", "localhost"] {
            assert!(g.host_allowed(ok), "{ok}");
        }
        for bad in [
            "evil.example",
            "app.localhost",
            "localhost.",
            "127.0.0.1.nip.io",
        ] {
            assert!(!g.host_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn configured_and_listed_hosts_are_answered() {
        let g = guard("MyHost.lan", &["Cereyan.Example.com", "[fd00::1]"]);
        assert!(g.host_allowed("myhost.lan"));
        assert!(g.host_allowed("cereyan.example.com"));
        assert!(g.host_allowed("fd00::1"));
        assert!(!g.host_allowed("other.example.com"));
    }

    #[test]
    fn origins_must_be_the_request_own_or_listed() {
        let g = guard("127.0.0.1", &["cereyan.example.com"]);
        let own = Some("127.0.0.1:4200");
        assert!(g.origin_allowed("http://127.0.0.1:4200", own));
        assert!(g.origin_allowed("HTTP://127.0.0.1:4200", own));
        assert!(!g.origin_allowed("http://localhost:3000", own));
        assert!(!g.origin_allowed("http://127.0.0.1:3000", own));
        assert!(!g.origin_allowed("http://127.0.0.1", own));
        assert!(!g.origin_allowed("https://evil.example", own));
        assert!(!g.origin_allowed("null", own));
        assert!(!g.origin_allowed("", own));
        assert!(!g.origin_allowed("file://", own));
        assert!(!g.origin_allowed("chrome-extension://abc", own));
        assert!(!g.origin_allowed("http://127.0.0.1:4200/path", own));
        assert!(!g.origin_allowed("http://evil@127.0.0.1:4200", own));
        // Listed hosts on any scheme or port, whatever the Host.
        assert!(g.origin_allowed("https://cereyan.example.com", own));
        assert!(g.origin_allowed("http://cereyan.example.com:8080", None));
        // A Host without a port matches the origin's port, default or not.
        assert!(g.origin_allowed("https://127.0.0.1", Some("127.0.0.1")));
        assert!(g.origin_allowed("http://127.0.0.1:81", Some("127.0.0.1")));
        // Default ports stand in for an origin without one.
        assert!(g.origin_allowed("http://127.0.0.1", Some("127.0.0.1:80")));
        assert!(!g.origin_allowed("https://127.0.0.1", Some("127.0.0.1:80")));
        // No Host to compare against.
        assert!(!g.origin_allowed("http://127.0.0.1:4200", None));
    }

    #[test]
    fn allowed_hosts_entries_are_checked() {
        let config = |allowed: &[&str]| -> ServeConfig {
            serde_json::from_value(json!({"home": "/tmp/x", "allowed_hosts": allowed})).unwrap()
        };
        for ok in [
            &[][..],
            &["cereyan.example.com"],
            &["LOCALHOST", "10.0.0.1", "::1", "[fd00::1]", "my-host"],
        ] {
            assert!(check_config(&config(ok)).is_ok(), "{ok:?}");
        }
        for bad in [
            "cereyan.example.com:443",
            "https://cereyan.example.com",
            "cereyan.example.com/x",
            "*",
            ".example.com",
            "example..com",
            "example.com.",
            "",
            "a b",
        ] {
            let err = check_config(&config(&[bad])).unwrap_err().to_string();
            assert!(err.contains("allowed_hosts"), "{bad}: {err}");
        }
    }

    fn app(guard: Guard) -> Router {
        Router::new()
            .route(
                "/api/runs",
                get(|| async { "runs" }).post(|| async { "posted" }),
            )
            .fallback(|| async { "fallback" })
            .layer(axum::middleware::from_fn_with_state(Arc::new(guard), check))
    }

    async fn send(
        router: Router,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = router
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body =
            serde_json::from_slice(&body).unwrap_or_else(|_| json!(String::from_utf8_lossy(&body)));
        (status, body)
    }

    #[tokio::test]
    async fn refusals_name_the_value_and_the_setting() {
        let (status, body) = send(
            app(guard("127.0.0.1", &[])),
            Method::GET,
            "/api/runs",
            &[("host", "Tunnel.Example:4200")],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["host"], "tunnel.example");
        assert!(body["error"].as_str().unwrap().contains("allowed_hosts"));

        let (status, body) = send(
            app(guard("127.0.0.1", &["hidden.example"])),
            Method::POST,
            "/api/runs",
            &[
                ("host", "127.0.0.1:4200"),
                ("origin", "https://evil.example"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["origin"], "https://evil.example");
        let text = body.to_string();
        assert!(text.contains("CEREYAN_ALLOWED_HOSTS") && !text.contains("hidden.example"));
    }

    #[tokio::test]
    async fn every_path_is_guarded_and_good_requests_pass() {
        let g = || guard("127.0.0.1", &[]);
        for path in ["/api/runs", "/api/health", "/", "/webhook"] {
            let (status, _) = send(app(g()), Method::GET, path, &[("host", "evil.example")]).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        }
        let (status, body) = send(
            app(g()),
            Method::POST,
            "/api/runs",
            &[
                ("host", "localhost:4200"),
                ("origin", "http://localhost:4200"),
            ],
        )
        .await;
        assert_eq!((status, body), (StatusCode::OK, json!("posted")));
        // No Host and no Origin: not a browser.
        let (status, _) = send(app(g()), Method::POST, "/api/runs", &[]).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn refused_before_the_token_check() {
        async fn no_token(_req: Request<Body>, _next: Next) -> Response {
            StatusCode::UNAUTHORIZED.into_response()
        }
        let router = Router::new()
            .route("/api/runs", get(|| async { "runs" }))
            .layer(axum::middleware::from_fn(no_token))
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(guard("127.0.0.1", &[])),
                check,
            ));
        let (status, _) = send(
            router.clone(),
            Method::GET,
            "/api/runs",
            &[("host", "evil.example")],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(router, Method::GET, "/api/runs", &[("host", "127.0.0.1")]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn trusted_transport_skips_the_guard() {
        let router = app(guard("127.0.0.1", &[])).layer(axum::Extension(TrustedTransport));
        let (status, _) = send(
            router,
            Method::POST,
            "/api/runs",
            &[("host", "evil.example"), ("origin", "https://evil.example")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn guarded_under_a_base_path() {
        let service = crate::base_path::service(app(guard("127.0.0.1", &[])), "/cereyan");
        let req = |host: &str| {
            Request::builder()
                .uri("/cereyan/api/runs")
                .header("host", host)
                .body(Body::empty())
                .unwrap()
        };
        let resp = service.clone().oneshot(req("evil.example")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let resp = service.oneshot(req("127.0.0.1:4200")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
