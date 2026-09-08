use cereyan_core::{State, StateType};
use cereyan_store::{CreateRun, NewLog, Store};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};

fn open() -> (tempfile::TempDir, Store, i64) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let flow = store
        .upsert_flow("bench", "flow", "m", "/d", None, "[]", "{}")
        .unwrap();
    (dir, store, flow)
}

fn bench_bulk_create(c: &mut Criterion) {
    let mut group = c.benchmark_group("writer");
    group.sample_size(10);
    group.throughput(Throughput::Elements(10_000));
    group.bench_function("bulk create 10k runs", |b| {
        b.iter_batched(
            open,
            |(_dir, store, flow)| {
                let cmds: Vec<CreateRun> = (0..10_000)
                    .map(|i| CreateRun {
                        flow_id: flow,
                        name: format!("r{i}"),
                        parameters: "{}".into(),
                        tags: "[]".into(),
                        created_by: "bench".into(),
                        initial_state: Some(State::new(StateType::Scheduled)),
                        ..Default::default()
                    })
                    .collect();
                store.create_runs_bulk(cmds).unwrap();
            },
            BatchSize::PerIteration,
        )
    });
    group.throughput(Throughput::Elements(100_000));
    group.bench_function("append 100k log lines", |b| {
        b.iter_batched(
            || {
                let (dir, store, flow) = open();
                let (run, _) = store.create_run(flow, "r", "{}", "[]").unwrap();
                (dir, store, run)
            },
            |(_dir, store, run)| {
                for chunk in 0..100 {
                    let logs: Vec<NewLog> = (0..1000)
                        .map(|i| NewLog {
                            run_id: run,
                            task_run_id: None,
                            task_run_external_id: None,
                            level: 20,
                            logger: "bench".into(),
                            timestamp: (chunk * 1000 + i) as i64,
                            message: "line".into(),
                        })
                        .collect();
                    store.append_logs(logs).unwrap();
                }
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

criterion_group!(benches, bench_bulk_create);
criterion_main!(benches);
