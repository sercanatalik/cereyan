//! Serving every TCP route under a configured URL path, e.g. `/cereyan`.
//!
//! The prefix is stripped by a layer around the whole router, so it runs
//! before routing and every handler, the token check, and the UI fallback see
//! the same unprefixed paths as at the root. `Router::nest` is not used: in
//! axum 0.8 a router nested at `/cereyan` matches `/cereyan` but not
//! `/cereyan/`, the opposite of what the UI needs.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use tower::{Layer, Service};

use crate::ServerError;

/// The base path arrives normalised from Python: `""` for the root, or
/// `/segment[/segment…]` with no trailing slash, each segment made of
/// letters, digits, `-`, `_`, `.`, or `~` and not `.` or `..`.
pub fn check(base_path: &str) -> Result<(), ServerError> {
    if base_path.is_empty() {
        return Ok(());
    }
    let valid = base_path.strip_prefix('/').is_some_and(|rest| {
        rest.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        })
    });
    if valid {
        Ok(())
    } else {
        Err(ServerError::Config(format!(
            "invalid base path {base_path:?}: expected \"\" or /segment[/segment...]"
        )))
    }
}

/// `router` served under `base_path`; at the root it is served unchanged.
pub fn service(
    router: Router,
    base_path: &str,
) -> impl Service<Request, Response = Response, Error = Infallible, Future: Send> + Clone + Send + 'static
{
    axum::middleware::from_fn_with_state(Arc::<str>::from(base_path), strip).layer(router)
}

/// `/` and `{base}` redirect to `{base}/`; `{base}/…` continues with the
/// prefix stripped; any other path is 404.
async fn strip(State(base): State<Arc<str>>, mut req: Request, next: Next) -> Response {
    if base.is_empty() {
        return next.run(req).await;
    }
    let path = req.uri().path();
    if path == "/" || path == &*base {
        // Temporary, so a browser does not remember it across a base path change.
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, format!("{base}/"))],
        )
            .into_response();
    }
    let Some(rest) = path.strip_prefix(&*base).filter(|r| r.starts_with('/')) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let target = match req.uri().query() {
        Some(q) => format!("{rest}?{q}"),
        None => rest.to_string(),
    };
    match target.parse::<Uri>() {
        Ok(uri) => {
            *req.uri_mut() = uri;
            next.run(req).await
        }
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::routing::get;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route(
                "/api/health",
                get(|uri: Uri| async move { uri.to_string() }),
            )
            .fallback(|uri: Uri| async move { format!("fallback {uri}") })
    }

    async fn get_path(base: &str, path: &str) -> (StatusCode, Option<String>, String) {
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        let resp = service(app(), base).oneshot(req).await.unwrap();
        let status = resp.status();
        let location = resp
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap().to_string());
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, location, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn nested_paths_reach_handlers_unprefixed() {
        let (status, _, body) = get_path("/cereyan", "/cereyan/api/health?x=1").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "/api/health?x=1"));
        let (status, _, body) = get_path("/cereyan", "/cereyan/runs/42").await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "fallback /runs/42")
        );
        let (status, _, body) = get_path("/cereyan", "/cereyan/").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "fallback /"));
        let (_, _, body) = get_path("/a/b", "/a/b/api/health").await;
        assert_eq!(body, "/api/health");
    }

    #[tokio::test]
    async fn root_and_bare_base_redirect() {
        for path in ["/", "/cereyan"] {
            let (status, location, _) = get_path("/cereyan", path).await;
            assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{path}");
            assert_eq!(location.as_deref(), Some("/cereyan/"), "{path}");
        }
    }

    #[tokio::test]
    async fn paths_outside_the_base_are_404() {
        for path in [
            "/elsewhere",
            "/api/health",
            "/cereyanx",
            "/cereyanx/api/health",
        ] {
            assert_eq!(
                get_path("/cereyan", path).await.0,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn root_base_serves_unchanged() {
        let (status, _, body) = get_path("", "/api/health").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "/api/health"));
        let (_, _, body) = get_path("", "/").await;
        assert_eq!(body, "fallback /");
    }

    #[test]
    fn check_accepts_only_normalised_values() {
        for ok in ["", "/cereyan", "/a/b.c~d_e-f"] {
            assert!(check(ok).is_ok(), "{ok}");
        }
        for bad in [
            "/",
            "cereyan",
            "/cereyan/",
            "/a//b",
            "/a/../b",
            "/./a",
            "/a b",
            "/a?x",
            "/{x}",
        ] {
            assert!(check(bad).is_err(), "{bad}");
        }
    }
}
