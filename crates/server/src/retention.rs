//! Hourly retention: delete logs and events older than `retain_days` in
//! batches of 5,000 rows, then, when `retain_runs_days` is set, expired
//! terminal runs in batches of 500, vacuuming incrementally after each batch.
//! The same pass writes a scheduled backup when one is due.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use cereyan_core::{now_micros, State, StateType};
use tokio::sync::watch;

use crate::state::AppState;

pub const BATCH: i64 = 5_000;
/// Runs per transaction: a run cascades to dozens of rows, so this is the same
/// order of work as a log batch.
pub const RUN_BATCH: i64 = 500;
const MICROS_PER_DAY: i64 = 86_400 * 1_000_000;
const MICROS_PER_HOUR: i64 = 3_600 * 1_000_000;

/// What one pass did.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Checkpoint files removed.
    pub checkpoints: usize,
    pub logs: usize,
    pub events: usize,
    pub runs: usize,
    pub backup: Option<PathBuf>,
}

/// One pass: logs and events, then runs, then a backup when due.
pub fn run_once(state: &AppState) -> Outcome {
    let mut out = Outcome::default();
    let days = state.retain_days.load(Ordering::Relaxed).max(0);
    if days > 0 {
        let before = now_micros() - days * MICROS_PER_DAY;
        for (table, slot) in [("log", 0usize), ("event", 1usize)] {
            loop {
                let n = match state.store.delete_expired(table, before, BATCH) {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if slot == 0 {
                    out.logs += n;
                } else {
                    out.events += n;
                }
                let _ = state.store.incremental_vacuum(2000);
                if (n as i64) < BATCH {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    out.runs = expire_runs(state);
    out.checkpoints = expire_checkpoints(state);
    out.backup = backup_if_due(state);
    out
}

/// Remove checkpoint files older than `retain_checkpoints_days` and forget the
/// references of terminal runs that ended before then; returns the files removed.
pub fn expire_checkpoints(state: &AppState) -> usize {
    let days = state.retain_checkpoints_days.load(Ordering::Relaxed);
    if days <= 0 {
        return 0;
    }
    let before = now_micros() - days * MICROS_PER_DAY;
    let _ = state.store.clear_checkpoints_before(before);
    let cutoff = std::time::UNIX_EPOCH + Duration::from_micros(before.max(0) as u64);
    let dir = state.config.home.join("storage");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("ckpt-") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| m < cutoff)
            .unwrap_or(false);
        if old && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Delete expired terminal runs under the run retention settings; returns the count.
pub fn expire_runs(state: &AppState) -> usize {
    let run_days = state.retain_runs_days.load(Ordering::Relaxed);
    if run_days <= 0 {
        return 0;
    }
    let failed_days = match state.retain_failed_runs_days.load(Ordering::Relaxed) {
        d if d > 0 => d,
        _ => run_days,
    };
    let keep = state.keep_last_runs_per_flow.load(Ordering::Relaxed).max(0);
    let now = now_micros();
    let before = now - run_days * MICROS_PER_DAY;
    let failed_before = now - failed_days * MICROS_PER_DAY;
    let mut total = 0;
    loop {
        let rows = match state
            .store
            .delete_expired_runs(before, failed_before, keep, RUN_BATCH)
        {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("warning: run retention stopped: {e}");
                break;
            }
        };
        let n = rows.len();
        for (id, flow_id, state_type) in rows {
            let previous = StateType::parse(&state_type).map(State::new);
            state.index.remove_run(id, previous.as_ref(), flow_id);
        }
        total += n;
        let _ = state.store.incremental_vacuum(2000);
        if (n as i64) < RUN_BATCH {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    total
}

/// Write a scheduled copy when `backup_every` is set and the last one is older
/// than that many hours, or none was ever written.
pub fn backup_if_due(state: &AppState) -> Option<PathBuf> {
    let every = state.backup_every.load(Ordering::Relaxed);
    if every <= 0 {
        return None;
    }
    let last = state
        .store
        .kv_get("backup.last_at")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if last > 0 && now_micros() - last < every * MICROS_PER_HOUR {
        return None;
    }
    match run_backup(state) {
        Ok(path) => Some(path),
        Err(e) => {
            eprintln!("warning: scheduled backup failed: {e}");
            None
        }
    }
}

/// Write a copy now, record it as the last backup, and prune beyond `backup_keep`.
/// Shared by the schedule, `POST /api/database/backup`, and so `cereyan backup`.
pub fn run_backup(state: &AppState) -> Result<PathBuf, cereyan_store::StoreError> {
    let path = state.store.backup()?;
    let _ = state
        .store
        .kv_set("backup.last_at", &now_micros().to_string());
    let _ = state
        .store
        .kv_set("backup.last_path", &path.display().to_string());
    let keep = state.backup_keep.load(Ordering::Relaxed).max(0) as usize;
    if let Err(e) = state.store.prune_backups(keep) {
        eprintln!("warning: could not prune old backups: {e}");
    }
    Ok(path)
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
