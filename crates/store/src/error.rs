use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("the cereyan database at {home} is locked by another process (PID {holder}); a server owns the database")]
    Locked { home: String, holder: String },
    #[error(
        "database schema version {db} is newer than this library supports ({lib}); upgrade cereyan"
    )]
    Downgrade { db: i64, lib: i64 },
    #[error("transition rejected: {0}")]
    Rejected(&'static str),
    #[error("transition rejected: {reason}")]
    RejectedWith {
        reason: &'static str,
        current: Option<cereyan_core::State>,
    },
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("invalid {0}")]
    Invalid(String),
    #[error("writer thread is gone")]
    WriterGone,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
