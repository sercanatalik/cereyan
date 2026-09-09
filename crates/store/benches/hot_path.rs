//! The write path a served task run actually travels: `transition` on both
//! tables, and `apply_report` over the event sequence an engine child sends.
//!
//! These attribute cost inside the store, with no server, engine child, or
//! HTTP in the measurement. `benches/e2e.py` is the end-to-end gate; this is
//! what tells you which part of the store moved.

use cereyan_core::{new_id, Id, State, StateType};
use cereyan_store::{NewLog, ReportEvent, Store};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};

fn open() -> (tempfile::TempDir, Store, i64) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let flow = store
        .upsert_flow("bench", "flow", "m", "/d", None, "[]", "{}")
        .unwrap();
    (dir, store, flow)
}

/// The report events one successful short task run produces.
///
/// This mirrors what the engine runner emits, and is the reason the numbers
/// below mean anything: `_create_task_run` sends the creation and the Pending
/// transition, `_run_task_attempts` sends Running then Completed and logs a
/// line on either side. Both live in `python/cereyan/engine/runner.py`; if
/// they change what they emit, this fixture has to change with them.
fn short_task_events(run_id: i64, index: usize, seq: &mut i64) -> Vec<ReportEvent> {
    let external_id = new_id();
    let mut next = || {
        *seq += 1;
        *seq
    };
    vec![
        ReportEvent::TaskRunCreated {
            seq: next(),
            external_id,
            name: "noop".into(),
            task_key: "noop".into(),
            dynamic_key: format!("noop-{index}"),
            parents: Vec::new(),
        },
        ReportEvent::TaskRunTransition {
            seq: next(),
            external_id,
            state: State::new(StateType::Pending),
            force: false,
        },
        ReportEvent::TaskRunTransition {
            seq: next(),
            external_id,
            state: State::new(StateType::Running),
            force: false,
        },
        ReportEvent::Logs {
            seq: next(),
            logs: log_lines(run_id, external_id, index),
        },
        ReportEvent::TaskRunTransition {
            seq: next(),
            external_id,
            state: State::new(StateType::Completed),
            force: false,
        },
    ]
}

fn log_lines(run_id: i64, external_id: Id, index: usize) -> Vec<NewLog> {
    ["started", "completed"]
        .iter()
        .map(|what| NewLog {
            run_id,
            task_run_id: None,
            task_run_external_id: Some(external_id),
            level: 20,
            logger: "cereyan.run".into(),
            timestamp: index as i64,
            message: format!("task noop-{index} {what}"),
        })
        .collect()
}

/// Runs and task runs walked through one legal lifecycle each, one store call
/// per transition. Entities are created in the setup phase so the timed
/// section is transitions only.
///
/// This is the offline path's shape: `StoreBackend` in
/// `python/cereyan/engine/backends.py` calls `transition_task_run` once per
/// transition, so each one pays a channel round trip and its own group commit.
/// Measured against `apply_report wide` below, that overhead is most of the
/// number here — so this benchmark tells you what an offline transition costs,
/// not what `transition()`'s SQL costs. For the latter, read `apply_report`,
/// where the same work happens many times inside one write command.
fn bench_transition(c: &mut Criterion) {
    const N: usize = 200;
    let mut group = c.benchmark_group("hot_path");
    group.sample_size(10);
    // Three transitions per entity: the unit is the transition.
    group.throughput(Throughput::Elements(N as u64 * 3));

    group.bench_function("transition run", |b| {
        b.iter_batched(
            || {
                let (dir, store, flow) = open();
                let ids: Vec<i64> = (0..N)
                    .map(|i| {
                        store
                            .create_run(flow, &format!("r{i}"), "{}", "[]")
                            .unwrap()
                            .0
                    })
                    .collect();
                (dir, store, ids)
            },
            |(_dir, store, ids)| {
                for id in ids {
                    for t in [StateType::Pending, StateType::Running, StateType::Completed] {
                        store.transition_run(id, State::new(t), false).unwrap();
                    }
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.bench_function("transition task_run", |b| {
        b.iter_batched(
            || {
                let (dir, store, flow) = open();
                let (run, _) = store.create_run(flow, "r", "{}", "[]").unwrap();
                let ids: Vec<i64> = (0..N)
                    .map(|i| {
                        store
                            .create_task_run(run, "noop", "noop", &format!("noop-{i}"))
                            .unwrap()
                            .0
                    })
                    .collect();
                (dir, store, ids)
            },
            |(_dir, store, ids)| {
                for id in ids {
                    for t in [StateType::Pending, StateType::Running, StateType::Completed] {
                        store.transition_task_run(id, State::new(t), false).unwrap();
                    }
                }
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

/// The served path: the same total work submitted one task run per report and
/// all at once. The gap between the two is the per-call cost (channel round
/// trip, group commit, the `report_seq` lookup) that batching amortises, which
/// is why the engine client batches at all.
///
/// `apply_report wide` is the number to watch for changes to the SQL inside
/// `transition`, `create_task_run` and `append_logs`: one write command covers
/// a whole task run's events, so per-call overhead is a small share of it.
fn bench_apply_report(c: &mut Criterion) {
    const N: usize = 500;
    let mut group = c.benchmark_group("hot_path");
    group.sample_size(10);
    group.throughput(Throughput::Elements(N as u64));

    group.bench_function("apply_report narrow", |b| {
        b.iter_batched(
            || {
                let (dir, store, flow) = open();
                let (run, _) = store.create_run(flow, "r", "{}", "[]").unwrap();
                let mut seq = 0i64;
                let reports: Vec<Vec<ReportEvent>> = (0..N)
                    .map(|i| short_task_events(run, i, &mut seq))
                    .collect();
                (dir, store, run, reports)
            },
            |(_dir, store, run, reports)| {
                for events in reports {
                    store.apply_report(run, events).unwrap();
                }
            },
            BatchSize::PerIteration,
        )
    });

    group.bench_function("apply_report wide", |b| {
        b.iter_batched(
            || {
                let (dir, store, flow) = open();
                let (run, _) = store.create_run(flow, "r", "{}", "[]").unwrap();
                let mut seq = 0i64;
                let events: Vec<ReportEvent> = (0..N)
                    .flat_map(|i| short_task_events(run, i, &mut seq))
                    .collect();
                (dir, store, run, events)
            },
            |(_dir, store, run, events)| {
                store.apply_report(run, events).unwrap();
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

/// One report, widening. Cost per task run should stay flat; a figure that
/// climbs with width is per-batch work growing faster than the batch. The
/// sweep is the only view that separates that from the fixed per-call cost the
/// narrow case above carries.
fn bench_apply_report_width(c: &mut Criterion) {
    let mut group = c.benchmark_group("hot_path/width");
    group.sample_size(10);
    for width in [100usize, 500, 2_000] {
        group.throughput(Throughput::Elements(width as u64));
        group.bench_with_input(BenchmarkId::from_parameter(width), &width, |b, &width| {
            b.iter_batched(
                || {
                    let (dir, store, flow) = open();
                    let (run, _) = store.create_run(flow, "r", "{}", "[]").unwrap();
                    let mut seq = 0i64;
                    let events: Vec<ReportEvent> = (0..width)
                        .flat_map(|i| short_task_events(run, i, &mut seq))
                        .collect();
                    (dir, store, run, events)
                },
                |(_dir, store, run, events)| {
                    store.apply_report(run, events).unwrap();
                },
                BatchSize::PerIteration,
            )
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_transition,
    bench_apply_report,
    bench_apply_report_width
);
criterion_main!(benches);
