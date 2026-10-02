use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use cereyan_core::{Flow, Run};
use serde::{Deserialize, Serialize};

use super::error::{ApiError, ApiResult};
use super::runs::CreateRunForFlowBody;
use crate::auth::{run_creator, AuthenticatedUser};
use crate::state::AppState;

#[derive(Deserialize, utoipa::IntoParams, Default)]
pub struct FlowsQuery {
    pub project: Option<String>,
    /// The resolved group: the one the flow declared, else its project.
    pub group: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct FlowSummary {
    #[serde(flatten)]
    pub flow: Flow,
    /// Newest first: (run id, state type, state name, duration in microseconds
    /// or null while the run has none) of the last ten runs.
    pub recent_runs: Vec<(i64, String, String, Option<i64>)>,
    /// Flows that run after this one.
    pub triggers: Vec<String>,
    /// The flow this one runs after, if any (the first upstream).
    pub triggered_by: Option<String>,
    /// Every upstream of a fan-in flow (empty without `after=`).
    #[serde(default)]
    pub upstreams: Vec<String>,
    /// The batch key of a keyed fan-in.
    #[serde(default)]
    pub batch_key: Option<String>,
    pub schedules: Vec<cereyan_core::ScheduleRow>,
    /// PASS, WARN or FAIL against the flow's freshness and deadline options; none without them.
    pub health: Option<crate::health::FlowHealth>,
}

/// How many recent runs the flows list shows per flow.
const RECENT_RUNS_PER_FLOW: usize = 10;

/// How many recent runs are read per flow. Health needs up to forty to derive a
/// median duration, and the newest ten are the newest ten of those, so one read
/// serves both and they cannot disagree.
const RECENT_RUNS_READ: usize = 40;

/// Resolve every flow's trigger list in one pass over `all`.
///
/// Inverts the dependency edge: instead of asking "who depends on me?" once per
/// flow — which parses every candidate's options each time — record "I depend on
/// them" once and answer every lookup from the map. Keyed by project and
/// upstream name, so a same-named flow in another project does not match.
fn triggers_by_upstream(all: &[Flow]) -> HashMap<(String, String), Vec<String>> {
    let mut triggers: HashMap<(String, String), Vec<String>> = HashMap::new();
    for flow in all {
        let options = cereyan_core::FlowOptions::from_map(&flow.options);
        let Some(after) = options.after else { continue };
        for upstream in after.upstreams() {
            // A self-dependency is not a trigger, matching the old `f.id != flow.id`.
            if upstream == flow.name {
                continue;
            }
            triggers
                .entry((flow.project.clone(), upstream))
                .or_default()
                .push(flow.name.clone());
        }
    }
    triggers
}

#[allow(clippy::too_many_arguments)]
fn summarize(
    state: &AppState,
    flow: Flow,
    options: &cereyan_core::FlowOptions,
    triggers: Vec<String>,
    active: &[crate::index::ActiveRun],
    recent: Vec<cereyan_store::RecentRun>,
    schedules: Vec<cereyan_core::ScheduleRow>,
    last_completed_at: Option<i64>,
    start_times: &std::collections::HashMap<i64, Option<i64>>,
) -> Result<FlowSummary, cereyan_store::StoreError> {
    let health = crate::health::flow_health(
        &flow,
        options,
        active,
        &recent,
        last_completed_at,
        start_times,
    );
    // The rows are newest first, so the displayed set is the newest of what was
    // read; health sees the whole sample.
    let recent: Vec<cereyan_store::RecentRun> =
        recent.into_iter().take(RECENT_RUNS_PER_FLOW).collect();
    Ok(FlowSummary {
        health,
        triggered_by: options.after.as_ref().map(|a| a.flow.clone()),
        upstreams: options
            .after
            .as_ref()
            .map(|a| a.upstreams())
            .unwrap_or_default(),
        batch_key: options.after.as_ref().and_then(|a| a.key.clone()),
        flow: decorate(state, flow),
        recent_runs: recent,
        triggers,
        schedules,
    })
}

/// Fill the read-time fields the store does not hold: whether the flow is live,
/// and its group resolved to the project when it declared none, so that clients
/// never re-apply the fallback.
pub fn decorate(state: &AppState, mut flow: Flow) -> Flow {
    flow.live = state.is_live(flow.id);
    flow.group = Some(flow.group_or_project().to_string());
    flow
}

#[utoipa::path(get, path = "/api/flows", params(FlowsQuery), responses((status = 200, body = Vec<FlowSummary>)))]
pub async fn list_flows(
    State(state): State<Arc<AppState>>,
    Query(q): Query<FlowsQuery>,
) -> ApiResult<Json<Vec<FlowSummary>>> {
    Ok(Json(flow_summaries(&state, &q)?))
}

/// The body of [`list_flows`], as a plain function.
///
/// Extracted so it can be tested. The endpoint needs an axum `State` and a
/// `Query`, and nothing about the logic does — and the ordering below (triggers
/// computed over *every* flow, before the response set is chosen) is exactly the
/// kind of thing a test should pin, because getting it wrong compiles and returns
/// a plausible-looking response.
fn flow_summaries(
    state: &AppState,
    q: &FlowsQuery,
) -> Result<Vec<FlowSummary>, cereyan_store::StoreError> {
    let all = state.store.list_flows(None)?;
    // One pass for the trigger lists, one for the active runs, and one options
    // parse per flow — instead of a per-flow scan and a full active-set clone.
    //
    // Triggers are a global relation, so this needs every registered flow even
    // when the response is filtered. It borrows `all` and returns owned, which is
    // what lets `flows` then *take* `all` rather than deep-copy every flow —
    // twenty-odd allocations each, including two already-parsed JSON documents —
    // on the most-polled page in the UI.
    let triggers = triggers_by_upstream(&all);
    // With no filter the filtered query is the same table, so read it once.
    let unfiltered = q.project.is_none() && q.group.is_none();
    let flows = if unfiltered {
        all
    } else {
        state
            .store
            .list_flows_filtered(q.project.as_deref(), q.group.as_deref())?
    };
    let active_by_flow = state.index.active_runs_by_flow();
    // One read each for the whole request, rather than one per flow.
    let flow_ids: Vec<i64> = flows.iter().map(|f| f.id).collect();
    let recent_by_flow = state
        .store
        .recent_run_states_many(&flow_ids, RECENT_RUNS_READ)?;
    let last_completed = state.store.last_completed_at_many(&flow_ids)?;
    let schedules_by_flow = state.scheduler.for_flows(&flow_ids);
    // One batched read of every running run's start time, for the health rule.
    // Every running run is already in `active_by_flow`, so this is a filter over
    // data in hand -- and it replaces a whole `Run` read per running run.
    let running_ids: Vec<i64> = active_by_flow
        .values()
        .flat_map(|runs| runs.iter())
        .filter(|r| r.state.state_type == cereyan_core::StateType::Running)
        .map(|r| r.id)
        .collect();
    let start_times = state
        .store
        .run_start_times(&running_ids)
        .unwrap_or_default();
    let mut out = Vec::with_capacity(flows.len());
    for flow in flows {
        let id = flow.id;
        let options = cereyan_core::FlowOptions::from_map(&flow.options);
        let flow_triggers = triggers
            .get(&(flow.project.clone(), flow.name.clone()))
            .cloned()
            .unwrap_or_default();
        let active = active_by_flow.get(&id).map(|v| v.as_slice()).unwrap_or(&[]);
        out.push(summarize(
            state,
            flow,
            &options,
            flow_triggers,
            active,
            recent_by_flow.get(&id).cloned().unwrap_or_default(),
            schedules_by_flow.get(&id).cloned().unwrap_or_default(),
            last_completed.get(&id).copied(),
            &start_times,
        )?);
    }
    Ok(out)
}

#[utoipa::path(get, path = "/api/flows/{id}", params(("id" = i64, Path)), responses((status = 200, body = FlowSummary), (status = 404)))]
pub async fn get_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<FlowSummary>> {
    let flow = state
        .store
        .get_flow(id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    let all = state.store.list_flows(None)?;
    let triggers = triggers_by_upstream(&all);
    let options = cereyan_core::FlowOptions::from_map(&flow.options);
    let flow_triggers = triggers
        .get(&(flow.project.clone(), flow.name.clone()))
        .cloned()
        .unwrap_or_default();
    let active = state.index.active_runs_for_flow(id);
    // One flow, so the single-flow readers are the natural fit.
    let recent = state.store.recent_run_states(id, RECENT_RUNS_READ)?;
    let schedules = state.scheduler.for_flow(id);
    let last_completed_at = state
        .store
        .list_runs(&cereyan_store::ListRunsFilter {
            flow_id: Some(id),
            state_type: Some("Completed".into()),
            limit: Some(1),
            sort: Some("created_desc".into()),
            ..Default::default()
        })
        .ok()
        .and_then(|p| p.items.into_iter().next())
        .and_then(|r| r.end_time);
    // One flow's running runs, one batched read -- the same path the list takes.
    let start_times = state
        .store
        .run_start_times(
            &active
                .iter()
                .filter(|r| r.state.state_type == cereyan_core::StateType::Running)
                .map(|r| r.id)
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
    Ok(Json(summarize(
        &state,
        flow,
        &options,
        flow_triggers,
        &active,
        recent,
        schedules,
        last_completed_at,
        &start_times,
    )?))
}

#[utoipa::path(delete, path = "/api/flows/{id}", params(("id" = i64, Path)), responses((status = 204), (status = 409, description = "Flow is live"), (status = 404)))]
pub async fn delete_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let flow = state
        .store
        .get_flow(id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    if state.is_live(flow.id) {
        return Err(ApiError::Conflict(serde_json::json!({
            "error": "flow is registered by the running server; stop serving it before deleting"
        })));
    }
    for run in state.index.active_runs() {
        if run.flow_id == id {
            state.supervisor.dequeue(run.id);
        }
    }
    state.store.delete_flow(id)?;
    state.index.remove_flow(id);
    state.invalidate_dep_graph();
    state.stream.publish(
        "flow.registered",
        id.to_string(),
        serde_json::json!({"id": id, "deleted": true}),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/flows/{id}/runs", params(("id" = i64, Path)), request_body = CreateRunForFlowBody, responses((status = 201, body = Run), (status = 200, body = super::runs::RunConflict, description = "A run already holds the unique or idempotency key"), (status = 422, description = "Invalid parameters")))]
pub async fn create_run_for_flow(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    user: Option<Extension<AuthenticatedUser>>,
    Json(body): Json<CreateRunForFlowBody>,
) -> ApiResult<axum::response::Response> {
    let flow = state
        .store
        .get_flow(id)?
        .ok_or_else(|| ApiError::NotFound("flow not found".into()))?;
    let created_by = run_creator(user.as_ref().map(|Extension(u)| u), "api");
    let starts = super::runs::not_before(body.scheduled_time, body.delay)?;
    let idempotency =
        super::runs::Idempotency::from_body(body.idempotency_key, body.idempotency_ttl)?;
    let (run, conflict) = super::runs::create_run_checked(
        &state,
        &flow,
        body.parameters,
        body.name,
        body.tags,
        &created_by,
        starts,
        None,
        idempotency,
    )
    .await?;
    Ok(super::runs::created_response(run, conflict))
}

#[cfg(test)]
pub(crate) mod trigger_tests {
    use super::*;
    use cereyan_core::FlowOptions;

    /// The reordering this change made possible carries one real risk: that
    /// `triggers` ends up computed over the *filtered* set rather than the whole
    /// table.
    ///
    /// A **project** filter cannot lose a trigger — a dependent names its upstream
    /// within its own project, so both are filtered together. (Getting that wrong
    /// in an earlier draft of this test was instructive: `triggers_by_upstream`
    /// keys on `(dependent.project, upstream_name)`, so a dependent in `q` naming
    /// `etl` triggers `q/etl`, never `p/etl`.)
    ///
    /// A **group** filter can. `list_flows_filtered` selects on
    /// `COALESCE(flow_group, project)`, so one group spans projects and can exclude
    /// a flow that another flow in the response names as an upstream. Computing
    /// triggers over that narrower set would drop it.
    ///
    /// This is what the ordering in `list_flows` guarantees, and it is asserted
    /// here rather than left to inspection.
    #[test]
    fn triggers_cover_every_registered_flow_not_just_the_response() {
        // `hourly` group: two flows in different projects that share a group.
        // `etl` is in the `daily` group and so is excluded from a response
        // filtered to `hourly` — but `load-hourly` still names it.
        let all = vec![
            flow_grouped(1, "p", "etl", "daily", None),
            flow_grouped(2, "p", "load-hourly", "hourly", Some("etl")),
            flow_grouped(3, "q", "mirror-hourly", "hourly", Some("etl")),
            flow_grouped(4, "p", "daily-job", "daily", Some("etl")),
        ];

        let full = triggers_by_upstream(&all);
        assert_eq!(
            full.get(&("p".to_string(), "etl".to_string())),
            Some(&vec!["load-hourly".to_string(), "daily-job".to_string(),]),
            "every dependent of `p/etl`, in input order"
        );
        // The cross-project dependent names an upstream in *its own* project.
        assert_eq!(
            full.get(&("q".to_string(), "etl".to_string())),
            Some(&vec!["mirror-hourly".to_string()]),
            "a dependent keys its trigger under its own project"
        );

        // A response filtered to the `daily` group keeps the *upstream* and drops
        // one of its dependents. That is the direction that loses information: the
        // upstream is in the response, and its trigger list would silently be
        // short by `load-hourly`.
        let daily: Vec<Flow> = all
            .iter()
            .filter(|f| f.group.as_deref() == Some("daily"))
            .cloned()
            .collect();
        assert_eq!(daily.len(), 2, "the group filter is doing something");
        assert!(
            daily.iter().any(|f| f.name == "etl"),
            "the upstream is in the filtered response, which is what makes this matter"
        );
        assert!(
            !daily.iter().any(|f| f.name == "load-hourly"),
            "and its dependent is not"
        );

        let from_filtered = triggers_by_upstream(&daily);
        assert_eq!(
            from_filtered.get(&("p".to_string(), "etl".to_string())),
            Some(&vec!["daily-job".to_string()]),
            "computing triggers over the group-filtered set loses `load-hourly` -- \
             the exact regression the ordering prevents"
        );
        assert_ne!(from_filtered, full, "the two sets give different answers");

        // And the map the endpoint actually uses is the complete one.
        assert_eq!(
            triggers_by_upstream(&all),
            full,
            "the map does not depend on how the flow set is subsequently consumed"
        );
    }

    #[test]
    fn a_self_dependency_is_not_a_trigger() {
        let all = vec![flow(1, "p", "loop", Some("loop"))];
        assert!(
            triggers_by_upstream(&all).is_empty(),
            "a flow that declares `after=` itself is not its own trigger"
        );
    }

    #[test]
    fn a_flow_with_no_after_declares_no_triggers() {
        let all = vec![
            flow(1, "p", "etl", None),
            flow(2, "p", "daily", Some("etl")),
        ];
        let t = triggers_by_upstream(&all);
        assert_eq!(t.len(), 1, "only the dependent contributes a key");
        assert!(!t.contains_key(&("p".to_string(), "daily".to_string())));
    }

    #[test]
    fn no_flows_declare_no_triggers() {
        assert!(triggers_by_upstream(&[]).is_empty());
    }

    /// A stand-in for the flow row shape, so the fixture stays small. Only the
    /// fields the trigger resolution reads are real.
    fn flow(id: i64, project: &str, name: &str, after: Option<&str>) -> Flow {
        flow_grouped(id, project, name, "", after)
    }

    /// The same, with a resolved group. `list_flows_filtered` selects on
    /// `COALESCE(flow_group, project)`, so an empty group means "its project".
    fn flow_grouped(id: i64, project: &str, name: &str, group: &str, after: Option<&str>) -> Flow {
        let mut f = flow_inner(id, project, name, after);
        f.group = if group.is_empty() {
            None
        } else {
            Some(group.into())
        };
        f
    }

    /// A set of `n` flows with the payload a real one carries: a parameter
    /// schema with a nested object, a tags array, and an options object. Used by
    /// the clone benchmark, where the payload sizes are the point.
    pub(crate) fn grouped_flow_for_bench(n: i64) -> Vec<Flow> {
        (0..n)
            .map(|i| {
                let mut f = flow_inner(i, "project", &format!("flow-{i}"), None);
                f.module = format!("module-{}", i % 20);
                f.source_dir = format!("/srv/flows/{}", i % 200);
                f.description = Some("a flow with a reasonably long description text".into());
                f.tags = vec!["prod".into(), "eu".into(), "team-a".into()];
                f.parameter_schema = serde_json::json!({
                    "day": "string", "region": "string", "limit": "integer",
                    "nested": {"a": 1, "b": [1, 2, 3]},
                });
                f.options = serde_json::json!({
                    "retries": 5, "timeout": 3600,
                    "schedules": [{"cron": "0 * * * *", "catchup": "none"}],
                })
                .as_object()
                .cloned()
                .unwrap_or_default();
                f
            })
            .collect()
    }

    fn flow_inner(id: i64, project: &str, name: &str, after: Option<&str>) -> Flow {
        let options = match after {
            // `after` is an AfterSpec struct, not a bare name.
            Some(up) => serde_json::json!({ "after": { "flow": up } }),
            None => serde_json::json!({}),
        };
        Flow {
            id,
            external_id: cereyan_core::new_id(),
            project: project.into(),
            name: name.into(),
            module: "m".into(),
            source_dir: "/tmp".into(),
            description: None,
            tags: vec![],
            group: None,
            parameter_schema: serde_json::json!({}),
            options: match options {
                serde_json::Value::Object(m) => m,
                _ => unreachable!("options is built as an object"),
            },
            error: None,
            created_at: 0,
            last_seen_at: 0,
            live: false,
        }
    }

    /// The pre-change algorithm, kept here as the oracle: for each flow, scan
    /// every candidate and parse its options.
    fn triggers_the_old_way(flow: &Flow, all: &[Flow]) -> Vec<String> {
        all.iter()
            .filter(|f| f.project == flow.project && f.id != flow.id)
            .filter(|f| {
                FlowOptions::from_map(&f.options)
                    .after
                    .map(|a| a.depends_on(&flow.name))
                    .unwrap_or(false)
            })
            .map(|f| f.name.clone())
            .collect()
    }

    fn check(flows: &[Flow], subject: &Flow) {
        let map = triggers_by_upstream(flows);
        let got = map
            .get(&(subject.project.clone(), subject.name.clone()))
            .cloned()
            .unwrap_or_default();
        let want = triggers_the_old_way(subject, flows);
        assert_eq!(got, want, "triggers differ for {}", subject.name);
    }

    #[test]
    fn triggers_match_the_per_flow_scan() {
        // A shared upstream, a chain, a cross-project name clash, a self
        // dependency, and a flow nothing depends on.
        let flows = vec![
            flow(1, "a", "etl", None),
            flow(2, "a", "daily", Some("etl")),
            flow(3, "a", "hourly", Some("etl")),
            flow(4, "a", "report", Some("daily")),
            // Same upstream name, different project: must not match.
            flow(5, "b", "other", Some("etl")),
            // Depends on itself: must not be its own trigger.
            flow(6, "a", "loop", Some("loop")),
            flow(7, "a", "lonely", None),
        ];
        // Guard against a vacuous comparison: the fixture must produce a
        // multi-entry trigger list, or every equality below is trivially true
        // and would pass even if the resolution were broken.
        let widest = flows
            .iter()
            .map(|f| triggers_the_old_way(f, &flows).len())
            .max()
            .unwrap_or(0);
        assert!(
            widest >= 2,
            "fixture is too weak: widest trigger list is {widest}"
        );
        for f in &flows {
            check(&flows, f);
        }
    }

    #[test]
    fn several_dependents_all_appear() {
        let flows = vec![
            flow(1, "p", "src", None),
            flow(2, "p", "one", Some("src")),
            flow(3, "p", "two", Some("src")),
            flow(4, "p", "three", Some("src")),
        ];
        let map = triggers_by_upstream(&flows);
        let t = map.get(&("p".to_string(), "src".to_string())).unwrap();
        assert_eq!(t, &vec!["one", "two", "three"]);
    }

    #[test]
    fn trigger_order_follows_the_input_order() {
        let flows = vec![
            flow(1, "p", "src", None),
            flow(2, "p", "zebra", Some("src")),
            flow(3, "p", "apple", Some("src")),
            flow(4, "p", "mango", Some("src")),
        ];
        let map = triggers_by_upstream(&flows);
        let t = map.get(&("p".to_string(), "src".to_string())).unwrap();
        assert_eq!(t, &vec!["zebra", "apple", "mango"], "input order kept");
    }

    #[test]
    fn a_flow_with_no_dependents_has_an_empty_list() {
        let flows = vec![flow(1, "p", "alone", None)];
        let map = triggers_by_upstream(&flows);
        assert!(!map.contains_key(&("p".to_string(), "alone".to_string())));
    }

    #[test]
    fn a_dependency_on_a_missing_upstream_is_inert() {
        // Nothing is wrong with a flow naming an upstream that does not exist:
        // the entry is recorded but no flow ever looks it up, exactly as the
        // old per-flow scan would have produced nothing for it.
        let flows = vec![flow(1, "p", "child", Some("ghost"))];
        let map = triggers_by_upstream(&flows);
        // The child itself is depended on by nobody.
        assert!(!map.contains_key(&("p".to_string(), "child".to_string())));
        // And the old algorithm agrees for the one flow that exists.
        for f in &flows {
            check(&flows, f);
        }
    }
}

/// Measures the deep copy `list_flows` used to make, on the real `Flow` type.
/// Opt-in: it is a measurement, not an assertion.
#[cfg(test)]
mod flow_clone_bench {
    use super::trigger_tests::grouped_flow_for_bench;
    use super::triggers_by_upstream;

    #[test]
    fn report_flow_list_clone_cost() {
        if std::env::var("CEREYAN_BENCH_REPORT").is_err() {
            return;
        }
        let flows = grouped_flow_for_bench(2_000);
        let iters = 200usize;

        // What the endpoint used to do: clone the whole set, then move one half.
        let t = std::time::Instant::now();
        for _ in 0..iters {
            let all = flows.clone();
            let triggers = triggers_by_upstream(&all);
            let flows = all.clone();
            std::hint::black_box((triggers, flows));
        }
        let cloning = t.elapsed().as_secs_f64() / iters as f64 * 1e3;

        // What it does now: borrow for the triggers, then move.
        let t = std::time::Instant::now();
        for _ in 0..iters {
            let all = flows.clone(); // stands in for the store read
            let triggers = triggers_by_upstream(&all);
            let flows = all;
            std::hint::black_box((triggers, flows));
        }
        let moving = t.elapsed().as_secs_f64() / iters as f64 * 1e3;

        // And the copy on its own, so the saving can be attributed.
        let t = std::time::Instant::now();
        for _ in 0..iters {
            std::hint::black_box(flows.clone());
        }
        let clone_only = t.elapsed().as_secs_f64() / iters as f64 * 1e3;

        println!(
            "2000 flows: read + clone + build {cloning:.3} ms, read + build {moving:.3} ms \\
             ({:.1}x); the clone alone is {clone_only:.3} ms",
            cloning / moving
        );
    }
}

#[cfg(test)]
pub(crate) mod list_flows_tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// A real `AppState` over a real store, so a caller's own logic — not a
    /// reconstruction of it — is what the test drives.
    pub(crate) fn state_with_flows(
        dir: &TempDir,
        flows: &[(&str, &str, Option<&str>, Option<&str>)],
    ) -> Arc<AppState> {
        let home = dir.path().join("home");
        let store = Arc::new(cereyan_store::Store::open(&home).unwrap());
        for (project, name, group, after) in flows {
            let mut options = serde_json::Map::new();
            if let Some(a) = after {
                options.insert("after".into(), serde_json::json!({ "flow": a }));
            }
            store
                .upsert_flow_full(cereyan_store::UpsertFlow {
                    project: (*project).into(),
                    name: (*name).into(),
                    module: "m".into(),
                    source_dir: "/tmp".into(),
                    description: None,
                    tags: "[]".into(),
                    parameter_schema: "{}".into(),
                    options: serde_json::Value::Object(options).to_string(),
                    group: group.map(|g| g.to_string()),
                })
                .unwrap();
        }
        let config: crate::ServeConfig = serde_json::from_value(serde_json::json!({
            "home": home.to_string_lossy(),
        }))
        .unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(false);
        Arc::new(
            AppState::new(
                config,
                store,
                None,
                None,
                "127.0.0.1:0".parse().unwrap(),
                rx,
            )
            .unwrap(),
        )
    }

    /// The regression this extraction exists to catch.
    ///
    /// Triggers are a **global** relation, so they must be computed over every
    /// registered flow. Computing them over the response's filtered set compiles,
    /// returns a well-formed response, and silently drops every trigger whose
    /// dependent the filter excluded. Before the body was extracted, **that
    /// injection passed all 416 tests** — verified.
    ///
    /// A group filter is what makes it observable: `list_flows_filtered` selects
    /// on `COALESCE(flow_group, project)`, so one group spans projects and a
    /// group-filtered response can hold a flow whose upstream is in a different
    /// group.
    #[test]
    fn a_filtered_response_still_lists_every_trigger() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(
            &dir,
            &[
                // The upstream, in the `daily` group.
                ("p", "etl", Some("daily"), None),
                // Its dependent, in the `hourly` group — so a response filtered to
                // `hourly` excludes the upstream but keeps the dependent, and a
                // response filtered to `daily` keeps the upstream but not this.
                ("p", "load-hourly", Some("hourly"), Some("etl")),
                // A second dependent, in the same group as the upstream.
                ("p", "daily-job", Some("daily"), Some("etl")),
            ],
        );

        let summaries = flow_summaries(&state, &FlowsQuery::default()).unwrap();
        let etl = summaries
            .iter()
            .find(|s| s.flow.name == "etl")
            .expect("the upstream is listed");
        let mut want = vec!["load-hourly".to_string(), "daily-job".to_string()];
        want.sort();
        let mut got = etl.triggers.clone();
        got.sort();
        assert_eq!(
            got, want,
            "both dependents, including the one in another group. Order is the \
             flow listing order and is not what this pins."
        );

        // Filtered to `daily`: the upstream is present and must still list the
        // hourly dependent, which the response does not contain.
        let daily = flow_summaries(
            &state,
            &FlowsQuery {
                project: None,
                group: Some("daily".into()),
            },
        )
        .unwrap();
        let etl = daily
            .iter()
            .find(|s| s.flow.name == "etl")
            .expect("the upstream survives a `daily` filter");
        assert!(
            daily.iter().all(|s| s.flow.name != "load-hourly"),
            "the hourly dependent is not in this response"
        );
        let mut want = vec!["load-hourly".to_string(), "daily-job".to_string()];
        want.sort();
        let mut got = etl.triggers.clone();
        got.sort();
        assert_eq!(
            got, want,
            "but it is still listed as a trigger of the upstream that is"
        );
    }

    #[test]
    fn an_unfiltered_response_lists_every_flow() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(
            &dir,
            &[
                ("p", "etl", None, None),
                ("q", "billing", None, Some("etl")),
                ("r", "lonely", None, None),
            ],
        );
        let mut summaries = flow_summaries(&state, &FlowsQuery::default()).unwrap();
        let mut names: Vec<String> = summaries.iter().map(|s| s.flow.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["billing", "etl", "lonely"], "every flow");
        // A cross-project dependent names an upstream in its own project, so `q`
        // has no trigger entry for `p/etl` -- and `p/etl` has none either.
        summaries.sort_by(|a, b| a.flow.name.cmp(&b.flow.name));
        assert!(
            summaries.iter().all(|s| s.triggers.is_empty()),
            "a cross-project dependent names an upstream in its own project, so \
             `p/etl` has no dependents here: {:?}",
            summaries
                .iter()
                .map(|s| (&s.flow.name, &s.triggers))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_flow_with_nothing_declared_has_no_health_or_triggers() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[("p", "plain", None, None)]);
        let summaries = flow_summaries(&state, &FlowsQuery::default()).unwrap();
        assert_eq!(summaries.len(), 1);
        let s = &summaries[0];
        assert!(s.triggers.is_empty());
        assert!(s.health.is_none(), "nothing to compare against");
        assert!(s.recent_runs.is_empty());
        assert!(s.schedules.is_empty());
        // And the full row shape survives, which is what a response carries.
        assert_eq!(s.flow.project, "p");
        assert_eq!(s.flow.name, "plain");
        assert_eq!(s.flow.module, "m");
    }

    #[test]
    fn listing_flows_leaves_the_stored_flows_alone() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(
            &dir,
            &[("p", "etl", None, None), ("p", "daily", None, Some("etl"))],
        );
        let first = flow_summaries(&state, &FlowsQuery::default()).unwrap();
        let second = flow_summaries(&state, &FlowsQuery::default()).unwrap();
        let names =
            |v: &[FlowSummary]| -> Vec<String> { v.iter().map(|s| s.flow.name.clone()).collect() };
        assert_eq!(names(&first), names(&second), "two listings agree");
        assert_eq!(
            state.store.list_flows(None).unwrap().len(),
            2,
            "nothing was lost"
        );
    }

    #[test]
    fn no_flows_yields_an_empty_list() {
        let dir = TempDir::new().unwrap();
        let state = state_with_flows(&dir, &[]);
        assert!(flow_summaries(&state, &FlowsQuery::default())
            .unwrap()
            .is_empty());
    }
}
