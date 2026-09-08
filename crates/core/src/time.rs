//! All timestamps are integer microseconds since the Unix epoch, UTC.

use std::time::{SystemTime, UNIX_EPOCH};

pub type Micros = i64;

pub fn now_micros() -> Micros {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

pub const MICROS_PER_SECOND: i64 = 1_000_000;
