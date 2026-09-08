use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use cereyan_store::StoreError;
use serde::Serialize;

#[derive(Serialize, utoipa::ToSchema)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    BadRequest(String),
    Unprocessable(String),
    Conflict(serde_json::Value),
    Internal(String),
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(what) => ApiError::NotFound(format!("{what} not found")),
            StoreError::Invalid(msg) => ApiError::BadRequest(msg),
            other => ApiError::Internal(other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, serde_json::json!({"error": m})),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, serde_json::json!({"error": m})),
            ApiError::Unprocessable(m) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                serde_json::json!({"error": m}),
            ),
            ApiError::Conflict(v) => (StatusCode::CONFLICT, v),
            ApiError::Internal(m) => {
                eprintln!("cereyan api error: {m}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": m}),
                )
            }
        };
        (status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
