//! Settings: resource totals and the engine saturation warning.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use cereyan_core::FlowOptions;
use serde::{Deserialize, Serialize};

use super::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Serialize, utoipa::ToSchema)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub pid: u32,
    pub version: String,
    /// UI title in effect: `[ui] title` from cereyan.toml, or `cereyan`.
    pub title: String,
    pub home: String,
    pub served_dir: Option<String>,
    pub database_path: String,
    pub database_bytes: u64,
    pub wal_bytes: u64,
    #[schema(value_type = Object)]
    pub resources: serde_json::Value,
    pub max_engines: usize,
    pub engine_max_runs: u32,
    pub crash_retries_default: i64,
    pub catchup_default: String,
    pub retain_days: i64,
    /// Days to keep terminal runs; 0 keeps them.
    pub retain_runs_days: i64,
    /// Days to keep Failed and Crashed runs; 0 means `retain_runs_days`.
    pub retain_failed_runs_days: i64,
    /// Runs per flow retention never deletes.
    pub keep_last_runs_per_flow: i64,
    /// Hours between scheduled backups; 0 is off.
    pub backup_every: i64,
    /// Scheduled copies kept.
    pub backup_keep: i64,
    /// Microseconds since the epoch of the last backup, scheduled or on demand.
    pub last_backup_at: Option<i64>,
    pub last_backup_path: Option<String>,
    /// `db-*.sqlite` copies under the backup directory.
    pub backups: usize,
    pub email_configured: bool,
    pub secret_key_present: bool,
    pub secret_key_missing: bool,
    pub custom_routes: Vec<crate::custom::RouteSpec>,
    pub engine_saturation_risk: bool,
    pub saturation_flows: Vec<String>,
    pub saturation_reason: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SettingsPatch {
    #[serde(default)]
    pub resources: Option<HashMap<String, f64>>,
    #[serde(default)]
    pub retain_days: Option<i64>,
    #[serde(default)]
    pub retain_runs_days: Option<i64>,
    #[serde(default)]
    pub retain_failed_runs_days: Option<i64>,
    #[serde(default)]
    pub keep_last_runs_per_flow: Option<i64>,
    #[serde(default)]
    pub backup_every: Option<i64>,
    #[serde(default)]
    pub backup_keep: Option<i64>,
    #[serde(default)]
    pub crash_retries: Option<i64>,
    /// UI title; an empty string removes `[ui] title` and restores `cereyan`.
    #[serde(default)]
    pub title: Option<String>,
}

pub fn saturation(state: &AppState) -> (bool, Vec<String>, Option<String>) {
    let Ok(flows) = state.store.list_flows(None) else {
        return (false, vec![], None);
    };
    let mut capped: Vec<(String, i64)> = Vec::new();
    let mut short: Vec<String> = Vec::new();
    for f in flows.iter().filter(|f| state.is_live(f.id)) {
        let o = FlowOptions::from_map(&f.options);
        if let Some(c) = o.max_concurrent {
            if c > 0 {
                capped.push((f.name.clone(), c));
            }
        } else {
            // Uncapped flow whose schedule is shorter than its median duration.
            let median = state
                .store
                .median_run_duration(f.id)
                .ok()
                .flatten()
                .unwrap_or(0);
            if median > 0 {
                for row in state.scheduler.for_flow(f.id) {
                    if let cereyan_core::schedule::Schedule::Interval { interval, .. } =
                        row.schedule
                    {
                        if (interval * 1e6) < median as f64 {
                            short.push(f.name.clone());
                        }
                    }
                }
            }
        }
    }
    let total: i64 = capped.iter().map(|(_, c)| c).sum();
    if total >= state.supervisor.max_engines as i64 && !capped.is_empty() {
        return (
            true,
            capped.into_iter().map(|(n, _)| n).collect(),
            Some(format!(
                "declared max_concurrent caps total {total} with max_engines {}",
                state.supervisor.max_engines
            )),
        );
    }
    if !short.is_empty() {
        return (
            true,
            short,
            Some("schedule interval shorter than the median run duration".into()),
        );
    }
    (false, vec![], None)
}

#[utoipa::path(get, path = "/api/settings", responses((status = 200, body = Settings)))]
pub async fn get_settings(State(state): State<Arc<AppState>>) -> Json<Settings> {
    let (risk, flows, reason) = saturation(&state);
    let (db, wal) = state.store.file_sizes();
    let key_present = cereyan_store::secrets::key_exists(&state.config.home);
    let secrets_count = state.store.count_secret_variables().unwrap_or(0);
    Json(Settings {
        host: state.addr.ip().to_string(),
        port: state.addr.port(),
        pid: std::process::id(),
        version: state.config.version.clone(),
        title: state.title(),
        home: state.config.home.display().to_string(),
        served_dir: state
            .config
            .served_dir
            .as_ref()
            .map(|p| p.display().to_string()),
        database_path: state
            .config
            .home
            .join(cereyan_store::DB_FILE)
            .display()
            .to_string(),
        database_bytes: db,
        wal_bytes: wal,
        resources: state.supervisor.resources_snapshot(),
        max_engines: state.supervisor.max_engines,
        engine_max_runs: state.config.engine_max_runs,
        crash_retries_default: state
            .crash_retries_default
            .load(std::sync::atomic::Ordering::Relaxed),
        catchup_default: state.config.catchup_default.clone(),
        retain_days: state.retain_days.load(std::sync::atomic::Ordering::Relaxed),
        retain_runs_days: state
            .retain_runs_days
            .load(std::sync::atomic::Ordering::Relaxed),
        retain_failed_runs_days: state
            .retain_failed_runs_days
            .load(std::sync::atomic::Ordering::Relaxed),
        keep_last_runs_per_flow: state
            .keep_last_runs_per_flow
            .load(std::sync::atomic::Ordering::Relaxed),
        backup_every: state
            .backup_every
            .load(std::sync::atomic::Ordering::Relaxed),
        backup_keep: state.backup_keep.load(std::sync::atomic::Ordering::Relaxed),
        last_backup_at: state
            .store
            .kv_get("backup.last_at")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok()),
        last_backup_path: state.store.kv_get("backup.last_path").ok().flatten(),
        backups: state.store.list_backups().map(|v| v.len()).unwrap_or(0),
        email_configured: state.config.email.is_some(),
        secret_key_present: key_present,
        secret_key_missing: !key_present && secrets_count > 0,
        custom_routes: state.config.custom_routes.clone(),
        engine_saturation_risk: risk,
        saturation_flows: flows,
        saturation_reason: reason,
    })
}

/// Persist mutable settings into the served directory's cereyan.toml.
fn persist_toml(
    state: &AppState,
    resources: Option<&HashMap<String, f64>>,
    defaults: &[(&str, Option<i64>)],
    title: Option<&str>,
) -> Result<(), String> {
    let Some(dir) = &state.config.served_dir else {
        return Ok(());
    };
    let path = dir.join("cereyan.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml::Table = if text.trim().is_empty() {
        toml::Table::new()
    } else {
        text.parse::<toml::Table>().map_err(|e| e.to_string())?
    };
    if let Some(res) = resources {
        let table = doc
            .entry("resources")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(t) = table {
            for (k, v) in res {
                t.insert(k.clone(), toml::Value::Float(*v));
            }
        }
    }
    if defaults.iter().any(|(_, v)| v.is_some()) {
        let table = doc
            .entry("defaults")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(t) = table {
            for (key, value) in defaults {
                if let Some(v) = value {
                    t.insert((*key).into(), toml::Value::Integer(*v));
                }
            }
        }
    }
    match title {
        Some("") => {
            if let Some(toml::Value::Table(ui)) = doc.get_mut("ui") {
                ui.remove("title");
                if ui.is_empty() {
                    doc.remove("ui");
                }
            }
        }
        Some(title) => {
            let table = doc
                .entry("ui")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(t) = table {
                t.insert("title".into(), toml::Value::String(title.into()));
            }
        }
        None => {}
    }
    let rendered = toml::to_string(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&path, rendered).map_err(|e| e.to_string())
}

#[utoipa::path(patch, path = "/api/settings", request_body = SettingsPatch, responses((status = 200, body = Settings)))]
pub async fn patch_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsPatch>,
) -> ApiResult<Json<Settings>> {
    // Validate the title before anything changes, so a bad one leaves all as it was.
    let title = match body.title.as_deref() {
        Some(raw) => Some(crate::ui::normalize_title(Some(raw)).map_err(ApiError::Unprocessable)?),
        None => None,
    };
    if let Some(resources) = &body.resources {
        if resources.values().any(|v| *v < 0.0) {
            return Err(ApiError::Unprocessable(
                "resource totals must be non-negative".into(),
            ));
        }
        state.supervisor.set_totals(resources);
        for name in resources.keys() {
            state.mark_edited(&format!("resources.{name}"));
        }
        let merged = state.supervisor.resource_totals();
        let _ = state.store.kv_set(
            "settings.resources",
            &serde_json::to_string(&merged).unwrap_or_default(),
        );
    }
    for (key, value, slot) in [
        ("retain_days", body.retain_days, &state.retain_days),
        (
            "retain_runs_days",
            body.retain_runs_days,
            &state.retain_runs_days,
        ),
        (
            "retain_failed_runs_days",
            body.retain_failed_runs_days,
            &state.retain_failed_runs_days,
        ),
        (
            "keep_last_runs_per_flow",
            body.keep_last_runs_per_flow,
            &state.keep_last_runs_per_flow,
        ),
        ("backup_every", body.backup_every, &state.backup_every),
        ("backup_keep", body.backup_keep, &state.backup_keep),
    ] {
        let Some(v) = value else { continue };
        if v < 0 {
            return Err(ApiError::Unprocessable(format!(
                "{key} must be zero or more"
            )));
        }
        slot.store(v, std::sync::atomic::Ordering::Relaxed);
        let _ = state
            .store
            .kv_set(&format!("settings.{key}"), &v.to_string());
        state.mark_edited(&format!("defaults.{key}"));
    }
    if let Some(c) = body.crash_retries {
        if c < 0 {
            return Err(ApiError::Unprocessable(
                "crash_retries must be zero or more".into(),
            ));
        }
        state
            .crash_retries_default
            .store(c, std::sync::atomic::Ordering::Relaxed);
        let _ = state.store.kv_set("settings.crash_retries", &c.to_string());
        state.mark_edited("defaults.crash_retries");
    }
    if let Some(t) = title {
        *state.title.write().unwrap_or_else(|e| e.into_inner()) = t;
        state.mark_edited("ui.title");
    }
    persist_toml(
        &state,
        body.resources.as_ref(),
        &[
            ("retain_days", body.retain_days),
            ("retain_runs_days", body.retain_runs_days),
            ("retain_failed_runs_days", body.retain_failed_runs_days),
            ("keep_last_runs_per_flow", body.keep_last_runs_per_flow),
            ("backup_every", body.backup_every),
            ("backup_keep", body.backup_keep),
            ("crash_retries", body.crash_retries),
        ],
        body.title.as_deref().map(str::trim),
    )
    .map_err(ApiError::Internal)?;
    Ok(get_settings(State(state)).await)
}
