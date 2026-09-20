//! `GET /api/metrics` in the Prometheus text format and its JSON history.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::metrics::{self, Sample, SAMPLE_INTERVAL};
use crate::state::AppState;

pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

#[derive(Serialize, utoipa::ToSchema)]
pub struct MetricsHistory {
    pub interval_secs: u64,
    pub samples: Vec<Sample>,
}

#[utoipa::path(get, path = "/api/metrics", responses((status = 200, description = "Prometheus text exposition format, version 0.0.4", content_type = "text/plain")))]
pub async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    let st = state.clone();
    let body = tokio::task::spawn_blocking(move || metrics::render(&st))
        .await
        .unwrap_or_default();
    ([(header::CONTENT_TYPE, CONTENT_TYPE)], body).into_response()
}

#[utoipa::path(get, path = "/api/metrics/history", responses((status = 200, body = MetricsHistory)))]
pub async fn history(State(state): State<Arc<AppState>>) -> Json<MetricsHistory> {
    Json(MetricsHistory {
        interval_secs: SAMPLE_INTERVAL.as_secs(),
        samples: state.samples.all(),
    })
}
