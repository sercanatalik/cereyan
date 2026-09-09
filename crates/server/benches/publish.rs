//! The stream payload the served path builds on every transition.
//!
//! `AppState::publish_run` and `publish_task_run` call `serde_json::to_value`
//! on the whole entity for every accepted transition, before the broadcast
//! channel gets a chance to report that nobody is listening. On an unattended
//! overnight batch that work is paid in full and thrown away, so it is worth
//! knowing what it costs.

use cereyan_core::{State, StateType};
use cereyan_store::Store;
use criterion::{criterion_group, criterion_main, Criterion};

/// Fixtures are read back from the store rather than constructed field by
/// field, so they keep carrying real values as the model gains fields.
fn fixtures() -> (tempfile::TempDir, cereyan_core::Run, cereyan_core::TaskRun) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let flow = store
        .upsert_flow("bench", "flow", "m", "/d", None, "[]", "{}")
        .unwrap();
    let (run_id, _) = store
        .create_run(
            flow,
            "bench-run",
            r#"{"day": "2026-01-01"}"#,
            r#"["nightly"]"#,
        )
        .unwrap();
    let (task_id, _) = store
        .create_task_run(run_id, "noop", "noop", "noop-0")
        .unwrap();
    for t in [StateType::Pending, StateType::Running, StateType::Completed] {
        store.transition_run(run_id, State::new(t), false).unwrap();
        store
            .transition_task_run(task_id, State::new(t), false)
            .unwrap();
    }
    let run = store.get_run(run_id).unwrap().unwrap();
    let task_run = store.get_task_run(task_id).unwrap().unwrap();
    (dir, run, task_run)
}

fn bench_publish(c: &mut Criterion) {
    let (_dir, run, task_run) = fixtures();
    let mut group = c.benchmark_group("publish");
    group.bench_function("run payload", |b| {
        b.iter(|| serde_json::to_value(std::hint::black_box(&run)).unwrap())
    });
    group.bench_function("task_run payload", |b| {
        b.iter(|| serde_json::to_value(std::hint::black_box(&task_run)).unwrap())
    });
    group.finish();
}

criterion_group!(benches, bench_publish);
criterion_main!(benches);
