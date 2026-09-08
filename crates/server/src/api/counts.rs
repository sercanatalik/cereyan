use std::sync::Arc;

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;

use crate::index::Counts;
use crate::state::AppState;

#[derive(Deserialize, utoipa::IntoParams)]
pub struct CountsQuery {
    pub project: Option<String>,
}

#[utoipa::path(get, path = "/api/counts", params(CountsQuery), responses((status = 200, body = Counts)))]
pub async fn counts(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CountsQuery>,
) -> Json<Counts> {
    Json(state.index.counts(q.project.as_deref()))
}
