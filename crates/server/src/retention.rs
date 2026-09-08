//! Hourly retention: delete logs and events older than `retain_days` in
//! batches of 5,000 rows, vacuuming incrementally after each batch. Runs and
//! task runs are never touched.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use cereyan_core::now_micros;
use tokio::sync::watch;

use crate::state::AppState;

pub const BATCH: i64 = 5_000;

/// One pass; returns rows deleted per table.
pub fn run_once(state: &AppState) -> (usize, usize) {
    let days = state.retain_days.load(Ordering::Relaxed).max(0);
    if days == 0 {
        return (0, 0);
    }
    let before = now_micros() - days * 86_400 * 1_000_000;
    let mut totals = (0usize, 0usize);
    for (table, slot) in [("log", 0usize), ("event", 1usize)] {
        loop {
            let n = match state.store.delete_expired(table, before, BATCH) {
                Ok(n) => n,
                Err(_) => break,
            };
            if slot == 0 {
                totals.0 += n;
            } else {
                totals.1 += n;
            }
            let _ = state.store.incremental_vacuum(2000);
            if (n as i64) < BATCH {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    totals
}

pub async fn run_loop(state: Arc<AppState>, mut shutdown: watch::Receiver<bool>) {
    let interval = Duration::from_secs(state.config.retention_interval_secs.unwrap_or(3600));
    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.changed() => { if *shutdown.borrow() { return; } }
        }
        let st = state.clone();
        let _ = tokio::task::spawn_blocking(move || run_once(&st)).await;
    }
}
