//! HTTP API under `/api`, the SSE stream, engine endpoints, custom routes,
//! and the embedded UI fallback.

pub mod backfills;
pub mod counts;
pub mod database;
pub mod engine;
pub mod environment;
pub mod error;
pub mod flows;
pub mod logs;
pub mod metrics;
pub mod observability;
pub mod pause;
pub mod projects;
pub mod runs;
pub mod schedules;
pub mod settings;
pub mod stream;
pub mod task_runs;
pub mod vocabulary;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use utoipa::OpenApi;

use crate::state::AppState;
use crate::{custom, ui};

#[derive(Serialize, utoipa::ToSchema)]
pub struct ServerInfo {
    pub version: String,
    pub home: String,
    pub pid: u32,
    pub started_at: i64,
    /// Dialable URL including the base path, with no trailing slash.
    pub url: String,
    /// URL path every TCP route is served under; empty at the root.
    pub base_path: String,
    /// UI title in effect: `[ui] title` from cereyan.toml, or `cereyan`.
    pub title: String,
    pub served_dir: Option<String>,
    pub engines: Vec<serde_json::Value>,
    pub queued: usize,
    pub stream_seq: u64,
    /// Whether a token is required.
    pub auth: bool,
    /// Bound beyond loopback with no token required.
    pub exposed: bool,
    /// The file holding the generated token when the server generated one.
    pub token_file: Option<String>,
    /// The global pause while the scheduler is paused.
    pub paused: Option<crate::scheduler::Pause>,
}

#[utoipa::path(get, path = "/api/health", responses((status = 200, description = "Server is up")))]
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true}))
}

#[utoipa::path(get, path = "/api/server", responses((status = 200, body = ServerInfo)))]
async fn server_info(State(state): State<Arc<AppState>>) -> Json<ServerInfo> {
    Json(ServerInfo {
        version: state.config.version.clone(),
        home: state.config.home.display().to_string(),
        pid: std::process::id(),
        started_at: state.started_at,
        url: state.public_url(),
        base_path: state.config.base_path.clone(),
        title: state.title(),
        served_dir: state
            .config
            .served_dir
            .as_ref()
            .map(|p| p.display().to_string()),
        engines: state.supervisor.engines_snapshot(),
        queued: state.supervisor.queue_len(),
        stream_seq: state.stream.latest_seq(),
        auth: state.config.token.is_some(),
        exposed: state.exposed(),
        token_file: state.token_file(),
        paused: state.pause(),
    })
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "cereyan",
        description = "Local-first pipeline orchestrator API"
    ),
    paths(
        health,
        server_info,
        flows::list_flows,
        flows::get_flow,
        flows::delete_flow,
        flows::create_run_for_flow,
        runs::list_runs,
        runs::create_run,
        runs::get_run,
        runs::delete_run,
        runs::patch_attributes,
        runs::bulk_runs,
        runs::compare_runs,
        runs::retry_run,
        runs::run_state,
        runs::set_run_state,
        runs::run_tasks,
        runs::run_graph,
        runs::transition_run,
        runs::cancel_run,
        task_runs::list_task_runs,
        task_runs::get_task_run,
        logs::run_logs,
        logs::task_run_logs,
        counts::counts,
        metrics::metrics,
        metrics::history,
        stream::stream,
        engine::work,
        engine::report,
        engine::heartbeat,
        engine::failed,
        engine::acquire,
        engine::release,
        schedules::list_schedules,
        schedules::create_schedule,
        schedules::patch_schedule,
        schedules::delete_schedule,
        schedules::pause_schedule,
        schedules::resume_schedule,
        schedules::add_skips,
        schedules::delete_skip,
        schedules::upcoming_runs,
        schedules::pause_flow,
        schedules::resume_flow,
        schedules::preview_schedule,
        pause::get_scheduler,
        pause::pause_scheduler,
        pause::resume_scheduler,
        backfills::create_backfill,
        backfills::get_backfill,
        backfills::list_backfills,
        backfills::prefilter_backfill,
        backfills::cancel_backfill,
        settings::get_settings,
        settings::patch_settings,
        environment::get_environment,
        projects::list_projects,
        projects::get_project,
        projects::delete_project,
        database::get_database,
        database::reset_database,
        database::backup_database,
        vocabulary::get_vocabulary,
        observability::list_events,
        observability::get_event,
        observability::emit_event,
        observability::list_rules,
        observability::create_rule,
        observability::get_rule,
        observability::patch_rule,
        observability::delete_rule,
        observability::rule_firings,
        observability::rule_expectations,
        observability::list_artifacts,
        runs::resume_run,
        runs::run_input,
        observability::test_rule,
        observability::run_artifacts,
        observability::task_run_artifacts,
        observability::create_artifact,
        observability::list_variables,
        observability::create_variable,
        observability::get_variable,
        observability::patch_variable,
        observability::delete_variable,
    ),
    components(schemas(
        ServerInfo,
        crate::scheduler::Pause,
        pause::SchedulerStatus,
        pause::PauseBody,
        runs::RunConflict,
        runs::StateValue,
        runs::StateBody,
        cereyan_store::TaskStateRow,
        runs::RetryBody,
        runs::RetryResponse,
        crate::compare::RunComparison,
        crate::compare::ValueDiff,
        crate::compare::DurationDiff,
        crate::compare::TaskSide,
        crate::compare::TaskDiff,
        crate::compare::ArtifactDiff,
        crate::compare::CompareSummary,
        cereyan_core::Flow,
        cereyan_core::Run,
        cereyan_core::TaskRun,
        cereyan_core::Log,
        cereyan_core::State,
        cereyan_core::StateType,
        cereyan_store::RunsPage,
        cereyan_store::TaskRunsPage,
        cereyan_store::LogsPage,
        flows::FlowSummary,
        runs::CreateRunBody,
        runs::CreateRunForFlowBody,
        runs::TransitionBody,
        runs::TransitionRejected,
        runs::BulkBody,
        runs::BulkResult,
        crate::index::Counts,
        engine::WorkRequest,
        engine::WorkResponse,
        engine::ReportRequest,
        engine::ReportResponse,
        engine::HeartbeatRequest,
        engine::FailedRequest,
        crate::supervisor::WorkItem,
        error::ErrorBody,
        runs::RunGraph,
        runs::GraphNode,
        runs::GraphEdge,
        cereyan_core::ScheduleRow,
        cereyan_core::schedule::Schedule,
        cereyan_core::schedule::CatchupPolicy,
        cereyan_core::Backfill,
        cereyan_core::Event,
        cereyan_core::FlowOptions,
        cereyan_core::AfterSpec,
        cereyan_core::ScheduleDecl,
        schedules::ScheduleBody,
        schedules::SchedulePatchBody,
        schedules::PreviewBody,
        schedules::PreviewResponse,
        schedules::SkipBody,
        schedules::SkipResponse,
        schedules::DownstreamSkip,
        schedules::UpcomingItem,
        schedules::UpcomingRun,
        schedules::ProjectedFire,
        backfills::BackfillBody,
        backfills::BackfillStatus,
        backfills::PrefilterBody,
        settings::Settings,
        settings::SettingsPatch,
        environment::Environment,
        environment::Runtime,
        environment::ConfigEntry,
        environment::EnvVariable,
        projects::ProjectSummary,
        projects::ProjectPreview,
        database::DatabaseInfo,
        database::ResetBody,
        database::ResetResult,
        database::BackupResult,
        metrics::MetricsHistory,
        crate::metrics::Sample,
        cereyan_store::DeletedCounts,
        cereyan_store::TableCounts,
        vocabulary::Vocabulary,
        vocabulary::EventEntry,
        vocabulary::StateEntry,
        engine::AcquireRequest,
        engine::ReleaseRequest,
        cereyan_core::Resource,
        cereyan_core::RuleRow,
        cereyan_core::RuleSpec,
        cereyan_core::RuleMatch,
        cereyan_core::RuleAction,
        cereyan_core::RuleFiring,
        cereyan_core::ArtifactRow,
        cereyan_core::VariableRow,
        cereyan_store::EventsPage,
        observability::EmitEventBody,
        observability::RuleBody,
        observability::RulePatch,
        observability::ArtifactBody,
        observability::VariableBody,
        observability::VariablePatch,
        observability::VariableWithRaw,
        crate::custom::RouteSpec,
    ))
)]
pub struct ApiDoc;

async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/api/health", get(health))
        .route("/api/server", get(server_info))
        .route("/api/openapi.json", get(openapi))
        .route("/api/flows", get(flows::list_flows))
        .route(
            "/api/flows/{id}",
            get(flows::get_flow).delete(flows::delete_flow),
        )
        .route("/api/flows/{id}/runs", post(flows::create_run_for_flow))
        .route("/api/runs", get(runs::list_runs).post(runs::create_run))
        .route(
            "/api/runs/{id}",
            get(runs::get_run).delete(runs::delete_run),
        )
        .route("/api/runs/bulk", post(runs::bulk_runs))
        .route("/api/runs/compare", get(runs::compare_runs))
        .route(
            "/api/runs/{id}/attributes",
            axum::routing::patch(runs::patch_attributes).post(runs::patch_attributes),
        )
        .route("/api/runs/{id}/tasks", get(runs::run_tasks))
        .route("/api/runs/{id}/graph", get(runs::run_graph))
        .route(
            "/api/flows/{id}/schedules",
            get(schedules::list_schedules).post(schedules::create_schedule),
        )
        .route("/api/flows/{id}/upcoming", get(schedules::upcoming_runs))
        .route("/api/flows/{id}/pause", post(schedules::pause_flow))
        .route("/api/flows/{id}/resume", post(schedules::resume_flow))
        .route("/api/flows/{id}/backfill", post(backfills::create_backfill))
        .route("/api/flows/{id}/backfills", get(backfills::list_backfills))
        .route("/api/schedules/preview", post(schedules::preview_schedule))
        .route("/api/scheduler", get(pause::get_scheduler))
        .route("/api/scheduler/pause", post(pause::pause_scheduler))
        .route("/api/scheduler/resume", post(pause::resume_scheduler))
        .route(
            "/api/schedules/{sid}",
            axum::routing::patch(schedules::patch_schedule).delete(schedules::delete_schedule),
        )
        .route(
            "/api/schedules/{sid}/pause",
            post(schedules::pause_schedule),
        )
        .route(
            "/api/schedules/{sid}/resume",
            post(schedules::resume_schedule),
        )
        .route("/api/schedules/{sid}/skips", post(schedules::add_skips))
        .route(
            "/api/schedules/{sid}/skips/{fire}",
            axum::routing::delete(schedules::delete_skip),
        )
        .route("/api/backfills/{id}", get(backfills::get_backfill))
        .route(
            "/api/backfills/{id}/prefilter",
            post(backfills::prefilter_backfill),
        )
        .route(
            "/api/backfills/{id}/cancel",
            post(backfills::cancel_backfill),
        )
        .route(
            "/api/settings",
            get(settings::get_settings).patch(settings::patch_settings),
        )
        .route(
            "/api/settings/environment",
            get(environment::get_environment),
        )
        .route("/api/projects", get(projects::list_projects))
        .route(
            "/api/projects/{name}",
            get(projects::get_project).delete(projects::delete_project),
        )
        .route("/api/database", get(database::get_database))
        .route("/api/database/reset", post(database::reset_database))
        .route("/api/database/backup", post(database::backup_database))
        .route("/api/metrics", get(metrics::metrics))
        .route("/api/metrics/history", get(metrics::history))
        .route("/api/vocabulary", get(vocabulary::get_vocabulary))
        .route(
            "/api/events",
            get(observability::list_events).post(observability::emit_event),
        )
        .route("/api/events/{id}", get(observability::get_event))
        .route(
            "/api/rules",
            get(observability::list_rules).post(observability::create_rule),
        )
        .route(
            "/api/rules/{id}",
            get(observability::get_rule)
                .patch(observability::patch_rule)
                .delete(observability::delete_rule),
        )
        .route(
            "/api/artifacts",
            get(observability::list_artifacts).post(observability::create_artifact),
        )
        .route("/api/rules/{id}/firings", get(observability::rule_firings))
        .route(
            "/api/rules/{id}/expectations",
            get(observability::rule_expectations),
        )
        .route("/api/rules/{id}/test", post(observability::test_rule))
        .route(
            "/api/runs/{id}/artifacts",
            get(observability::run_artifacts),
        )
        .route(
            "/api/task-runs/{id}/artifacts",
            get(observability::task_run_artifacts),
        )
        .route(
            "/api/variables",
            get(observability::list_variables).post(observability::create_variable),
        )
        .route(
            "/api/variables/{name}",
            get(observability::get_variable)
                .patch(observability::patch_variable)
                .delete(observability::delete_variable),
        )
        .route("/api/resources/acquire", post(engine::acquire))
        .route("/api/resources/release", post(engine::release))
        .route("/api/runs/{id}/logs", get(logs::run_logs))
        .route("/api/runs/{id}/transition", post(runs::transition_run))
        .route("/api/runs/{id}/cancel", post(runs::cancel_run))
        .route("/api/runs/{id}/resume", post(runs::resume_run))
        .route("/api/runs/{id}/retry", post(runs::retry_run))
        .route(
            "/api/runs/{id}/state",
            get(runs::run_state).post(runs::set_run_state),
        )
        .route("/api/runs/{id}/input", get(runs::run_input))
        .route("/api/task-runs", get(task_runs::list_task_runs))
        .route("/api/task-runs/{id}", get(task_runs::get_task_run))
        .route("/api/task-runs/{id}/logs", get(logs::task_run_logs))
        .route("/api/counts", get(counts::counts))
        .route("/api/stream", get(stream::stream))
        .route("/api/engine/work", post(engine::work))
        .route("/api/engine/report", post(engine::report))
        .route("/api/engine/heartbeat", post(engine::heartbeat))
        .route("/api/engine/failed", post(engine::failed))
        .route(
            "/mcp",
            post(crate::mcp::handle_post)
                .get(crate::mcp::handle_get)
                .delete(crate::mcp::handle_delete),
        );
    // Engine reports can carry tens of thousands of log lines per batch.
    let api = api.layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024));
    let api = custom::attach(api, &state.config.custom_routes);
    // The check wraps the UI fallback too and decides by path and `auth_scope`,
    // so scope `api` leaves the UI open and scope `all` covers it. The guard
    // wraps everything, so a refused Host or Origin never reaches the token.
    let guard = Arc::new(crate::guard::Guard::new(&state.config));
    api.fallback(ui::serve)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_token,
        ))
        .layer(axum::middleware::from_fn_with_state(
            guard,
            crate::guard::check,
        ))
        .with_state(state)
}
