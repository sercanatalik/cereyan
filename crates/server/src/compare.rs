//! Compare two runs: what changed in parameters, tasks, errors and artifacts.
//! The left run is the baseline; "new" means present on the right only.

use std::collections::{BTreeSet, HashMap, HashSet};

use cereyan_core::{ArtifactRow, Log, Run, StateType, TaskRun};
use serde::Serialize;
use serde_json::Value;

/// Everything the comparison reads about one run.
pub struct RunBundle {
    pub run: Run,
    pub tasks: Vec<TaskRun>,
    /// Log lines at level 40 or above, oldest first.
    pub errors: Vec<Log>,
    pub artifacts: Vec<ArtifactRow>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct ValueDiff {
    pub key: String,
    pub left: Value,
    pub right: Value,
    pub changed: bool,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct DurationDiff {
    pub left: Option<i64>,
    pub right: Option<i64>,
    /// right − left, microseconds, when both are known.
    pub delta: Option<i64>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct TaskSide {
    pub id: i64,
    pub state: cereyan_core::State,
    pub duration: Option<i64>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct TaskDiff {
    /// The dynamic key, `step-0`, matched across the two runs.
    pub key: String,
    pub name: String,
    pub left: Option<TaskSide>,
    pub right: Option<TaskSide>,
    pub state_changed: bool,
    /// right − left duration, microseconds, when both ran.
    pub delta: Option<i64>,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct ArtifactDiff {
    pub key: Option<String>,
    pub kind: String,
    pub left: Option<i64>,
    pub right: Option<i64>,
    /// Present on one side only, or `data` differs.
    pub changed: bool,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct CompareSummary {
    pub parameters_changed: usize,
    pub attributes_changed: usize,
    pub tasks_state_changed: usize,
    /// Tasks whose duration moved by more than 10 percent and more than a second.
    pub tasks_duration_changed: usize,
    pub new_errors: usize,
    pub artifacts_changed: usize,
}

#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct RunComparison {
    pub left: Run,
    pub right: Run,
    pub same_flow: bool,
    pub parameters: Vec<ValueDiff>,
    pub attributes: Vec<ValueDiff>,
    pub duration: DurationDiff,
    pub tasks: Vec<TaskDiff>,
    /// The first task, in the left run's order, whose state differs or that exists on one side only.
    pub first_divergence: Option<String>,
    /// Error-level log lines and the failure message present only on the right.
    pub new_errors: Vec<String>,
    pub artifacts: Vec<ArtifactDiff>,
    pub summary: CompareSummary,
}

fn value_diffs(
    left: &serde_json::Map<String, Value>,
    right: &serde_json::Map<String, Value>,
) -> Vec<ValueDiff> {
    let keys: BTreeSet<&String> = left.keys().chain(right.keys()).collect();
    keys.into_iter()
        .map(|k| {
            let l = left.get(k).cloned().unwrap_or(Value::Null);
            let r = right.get(k).cloned().unwrap_or(Value::Null);
            ValueDiff {
                key: k.clone(),
                changed: l != r,
                left: l,
                right: r,
            }
        })
        .collect()
}

/// The task runs of the latest pass, in creation order.
fn latest_pass(tasks: &[TaskRun]) -> Vec<&TaskRun> {
    let last = tasks.iter().map(|t| t.pass).max().unwrap_or(0);
    let mut out: Vec<&TaskRun> = tasks.iter().filter(|t| t.pass == last).collect();
    out.sort_by_key(|t| (t.created_at, t.id));
    out
}

fn side(t: &TaskRun) -> TaskSide {
    TaskSide {
        id: t.id,
        state: t.state.clone(),
        duration: t.total_run_time,
    }
}

fn duration_moved(left: Option<i64>, right: Option<i64>) -> bool {
    match (left, right) {
        (Some(l), Some(r)) => {
            let delta = (r - l).abs();
            delta > 1_000_000 && delta as f64 > 0.1 * l.max(1) as f64
        }
        _ => false,
    }
}

fn failure_message(run: &Run) -> Option<&str> {
    matches!(run.state.state_type, StateType::Failed | StateType::Crashed)
        .then(|| run.state.message.as_deref())
        .flatten()
}

pub fn compare(left: &RunBundle, right: &RunBundle) -> RunComparison {
    let parameters = value_diffs(&left.run.parameters, &right.run.parameters);
    let attributes = value_diffs(&left.run.attributes, &right.run.attributes);
    let duration = DurationDiff {
        left: left.run.total_run_time,
        right: right.run.total_run_time,
        delta: match (left.run.total_run_time, right.run.total_run_time) {
            (Some(l), Some(r)) => Some(r - l),
            _ => None,
        },
    };

    let l_tasks = latest_pass(&left.tasks);
    let r_tasks = latest_pass(&right.tasks);
    let r_by_key: HashMap<&str, &TaskRun> = r_tasks
        .iter()
        .map(|t| (t.dynamic_key.as_str(), *t))
        .collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut tasks: Vec<TaskDiff> = Vec::new();
    for t in &l_tasks {
        seen.insert(t.dynamic_key.as_str());
        let other = r_by_key.get(t.dynamic_key.as_str()).copied();
        tasks.push(TaskDiff {
            key: t.dynamic_key.clone(),
            name: t.name.clone(),
            state_changed: other.is_none_or(|o| o.state.state_type != t.state.state_type),
            delta: match (t.total_run_time, other.and_then(|o| o.total_run_time)) {
                (Some(l), Some(r)) => Some(r - l),
                _ => None,
            },
            left: Some(side(t)),
            right: other.map(side),
        });
    }
    for t in r_tasks
        .iter()
        .filter(|t| !seen.contains(t.dynamic_key.as_str()))
    {
        tasks.push(TaskDiff {
            key: t.dynamic_key.clone(),
            name: t.name.clone(),
            left: None,
            right: Some(side(t)),
            state_changed: true,
            delta: None,
        });
    }
    let first_divergence = tasks
        .iter()
        .find(|t| t.state_changed)
        .map(|t| t.key.clone());

    let known: HashSet<&str> = left.errors.iter().map(|l| l.message.as_str()).collect();
    let mut new_errors: Vec<String> = Vec::new();
    for line in &right.errors {
        if !known.contains(line.message.as_str()) && !new_errors.contains(&line.message) {
            new_errors.push(line.message.clone());
        }
        if new_errors.len() >= 50 {
            break;
        }
    }
    if let Some(msg) = failure_message(&right.run) {
        if failure_message(&left.run) != Some(msg)
            && !new_errors.iter().any(|m| m == msg)
            && new_errors.len() < 50
        {
            new_errors.push(msg.to_string());
        }
    }

    let artifacts = artifact_diffs(&left.artifacts, &right.artifacts);

    let summary = CompareSummary {
        parameters_changed: parameters.iter().filter(|p| p.changed).count(),
        attributes_changed: attributes.iter().filter(|p| p.changed).count(),
        tasks_state_changed: tasks.iter().filter(|t| t.state_changed).count(),
        tasks_duration_changed: tasks
            .iter()
            .filter(|t| {
                duration_moved(
                    t.left.as_ref().and_then(|s| s.duration),
                    t.right.as_ref().and_then(|s| s.duration),
                )
            })
            .count(),
        new_errors: new_errors.len(),
        artifacts_changed: artifacts.iter().filter(|a| a.changed).count(),
    };
    RunComparison {
        same_flow: left.run.flow_id == right.run.flow_id,
        left: left.run.clone(),
        right: right.run.clone(),
        parameters,
        attributes,
        duration,
        tasks,
        first_divergence,
        new_errors,
        artifacts,
        summary,
    }
}

/// An artifact's identity across runs: its key, else its kind and its
/// position among that kind's keyless artifacts.
fn artifact_ids(rows: &[ArtifactRow]) -> Vec<(String, &ArtifactRow)> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    rows.iter()
        .map(|a| {
            let id = match &a.key {
                Some(k) => format!("key:{k}"),
                None => {
                    let n = counts.entry(a.kind.as_str()).or_insert(0);
                    let id = format!("{}#{}", a.kind, n);
                    *n += 1;
                    id
                }
            };
            (id, a)
        })
        .collect()
}

fn artifact_diffs(left: &[ArtifactRow], right: &[ArtifactRow]) -> Vec<ArtifactDiff> {
    let l = artifact_ids(left);
    let r = artifact_ids(right);
    let r_map: HashMap<&str, &ArtifactRow> = r.iter().map(|(id, a)| (id.as_str(), *a)).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for (id, a) in &l {
        seen.insert(id.as_str());
        let other = r_map.get(id.as_str()).copied();
        out.push(ArtifactDiff {
            key: a.key.clone(),
            kind: a.kind.clone(),
            left: Some(a.id),
            right: other.map(|o| o.id),
            changed: other.is_none_or(|o| o.data != a.data),
        });
    }
    for (id, a) in r.iter().filter(|(id, _)| !seen.contains(id.as_str())) {
        let _ = id;
        out.push(ArtifactDiff {
            key: a.key.clone(),
            kind: a.kind.clone(),
            left: None,
            right: Some(a.id),
            changed: true,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cereyan_core::State;
    use serde_json::json;

    fn run(id: i64, flow_id: i64, params: Value, state: StateType, msg: Option<&str>) -> Run {
        let mut r: Run = serde_json::from_value(json!({
            "id": id, "external_id": "00000000-0000-4000-8000-000000000001", "flow_id": flow_id, "flow_name": "etl", "project": "p",
            "group": "p", "name": format!("run-{id}"), "parameters": params, "tags": [], "attributes": {},
            "state": State::new(state), "failure_count": 0, "crash_count": 0, "created_at": 1,
            "created_by": "test", "report_seq": 0, "total_run_time": 10_000_000
        }))
        .unwrap();
        r.state.message = msg.map(|m| m.to_string());
        r
    }

    fn task(id: i64, run_id: i64, key: &str, pass: i64, state: StateType, dur: i64) -> TaskRun {
        serde_json::from_value(json!({
            "id": id, "external_id": "00000000-0000-4000-8000-000000000002", "run_id": run_id, "name": key.split('-').next().unwrap(),
            "task_key": key.split('-').next().unwrap(), "dynamic_key": key, "pass": pass,
            "state": State::new(state), "failure_count": 0, "crash_count": 0, "created_at": id,
            "total_run_time": dur, "run_name": "r", "flow_id": 1, "flow_name": "etl", "project": "p"
        }))
        .unwrap()
    }

    fn log(msg: &str) -> Log {
        serde_json::from_value(json!({"id": 1, "run_id": 1, "level": 40, "logger": "x", "timestamp": 1, "message": msg})).unwrap()
    }

    fn art(id: i64, key: Option<&str>, kind: &str, data: Value) -> ArtifactRow {
        serde_json::from_value(json!({"id": id, "external_id": "00000000-0000-4000-8000-000000000003", "run_id": 1, "kind": kind, "key": key, "data": data, "created_at": 1, "updated_at": 1})).unwrap()
    }

    #[test]
    fn parameters_tasks_errors_and_artifacts() {
        let left = RunBundle {
            run: run(
                1,
                1,
                json!({"day": "2026-09-01", "n": 1}),
                StateType::Completed,
                None,
            ),
            tasks: vec![
                task(1, 1, "load-0", 0, StateType::Completed, 1_000_000),
                task(2, 1, "check-0", 0, StateType::Completed, 5_000_000),
            ],
            errors: vec![log("known problem")],
            artifacts: vec![
                art(1, Some("rows"), "table", json!([1])),
                art(2, None, "markdown", json!("a")),
            ],
        };
        let right = RunBundle {
            run: run(
                2,
                1,
                json!({"day": "2026-09-02", "n": 1}),
                StateType::Failed,
                Some("boom"),
            ),
            tasks: vec![
                task(3, 2, "load-0", 0, StateType::Completed, 3_000_000),
                task(4, 2, "check-0", 0, StateType::Failed, 100_000),
                task(5, 2, "extra-0", 0, StateType::Completed, 100_000),
            ],
            errors: vec![log("known problem"), log("disk full")],
            artifacts: vec![
                art(3, Some("rows"), "table", json!([2])),
                art(4, None, "markdown", json!("a")),
            ],
        };
        let c = compare(&left, &right);
        assert!(c.same_flow);
        let day = c.parameters.iter().find(|p| p.key == "day").unwrap();
        assert!(day.changed && !c.parameters.iter().find(|p| p.key == "n").unwrap().changed);
        assert_eq!(c.tasks.len(), 3);
        assert_eq!(c.tasks[0].delta, Some(2_000_000));
        assert!(!c.tasks[0].state_changed && c.tasks[1].state_changed);
        assert_eq!(c.first_divergence.as_deref(), Some("check-0"));
        assert!(c.tasks[2].left.is_none() && c.tasks[2].state_changed);
        assert_eq!(
            c.new_errors,
            vec!["disk full".to_string(), "boom".to_string()]
        );
        assert_eq!(c.duration.delta, Some(0));
        assert_eq!(c.artifacts.len(), 2);
        assert!(c.artifacts[0].changed && !c.artifacts[1].changed);
        assert_eq!(
            (
                c.summary.parameters_changed,
                c.summary.tasks_state_changed,
                c.summary.tasks_duration_changed,
                c.summary.new_errors,
                c.summary.artifacts_changed
            ),
            (1, 2, 2, 2, 1)
        );
    }

    #[test]
    fn latest_pass_only_and_no_divergence() {
        let left = RunBundle {
            run: run(1, 1, json!({}), StateType::Completed, None),
            tasks: vec![
                task(1, 1, "a-0", 0, StateType::Failed, 1),
                task(2, 1, "a-0", 1, StateType::Completed, 1),
            ],
            errors: vec![],
            artifacts: vec![],
        };
        let right = RunBundle {
            run: run(2, 2, json!({}), StateType::Completed, None),
            tasks: vec![task(3, 2, "a-0", 0, StateType::Completed, 1)],
            errors: vec![],
            artifacts: vec![],
        };
        let c = compare(&left, &right);
        assert!(!c.same_flow);
        assert_eq!(c.tasks.len(), 1);
        assert!(c.first_divergence.is_none() && c.new_errors.is_empty());
    }
}
