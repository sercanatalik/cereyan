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
    pub crash_retries: Option<i64>,
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
    retain_days: Option<i64>,
    crash_retries: Option<i64>,
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
    if retain_days.is_some() || crash_retries.is_some() {
        let table = doc
            .entry("defaults")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(t) = table {
            if let Some(d) = retain_days {
                t.insert("retain_days".into(), toml::Value::Integer(d));
            }
            if let Some(c) = crash_retries {
                t.insert("crash_retries".into(), toml::Value::Integer(c));
            }
        }
    }
    let rendered = toml::to_string(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&path, rendered).map_err(|e| e.to_string())
}

#[utoipa::path(patch, path = "/api/settings", request_body = SettingsPatch, responses((status = 200, body = Settings)))]
pub async fn patch_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsPatch>,
) -> ApiResult<Json<Settings>> {
    if let Some(resources) = &body.resources {
        if resources.values().any(|v| *v < 0.0) {
            return Err(ApiError::Unprocessable(
                "resource totals must be non-negative".into(),
            ));
        }
        state.supervisor.set_totals(resources);
        let merged = state.supervisor.resource_totals();
        let _ = state.store.kv_set(
            "settings.resources",
            &serde_json::to_string(&merged).unwrap_or_default(),
        );
    }
    if let Some(days) = body.retain_days {
        if days < 0 {
            return Err(ApiError::Unprocessable(
                "retain_days must be zero or more".into(),
            ));
        }
        state
            .retain_days
            .store(days, std::sync::atomic::Ordering::Relaxed);
        let _ = state
            .store
            .kv_set("settings.retain_days", &days.to_string());
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
    }
    persist_toml(
        &state,
        body.resources.as_ref(),
        body.retain_days,
        body.crash_retries,
    )
    .map_err(ApiError::Internal)?;
    Ok(get_settings(State(state)).await)
}
