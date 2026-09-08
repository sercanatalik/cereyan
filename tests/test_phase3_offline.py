"""Events, artifacts, variables, and code rules on the offline path."""

import json

import pytest

from cereyan import App, Variable, artifacts, emit_event, flow, task
from cereyan.exceptions import CereyanError


def events(store, name=None):
    return json.loads(store.query_events(json.dumps({"name": name, "limit": 100, "ascending": True})))["items"]


def test_offline_run_events_are_recorded_in_order(store):
    @task
    def t():
        return 1

    @flow
    def f():
        t()
        raise ValueError("bad")

    with pytest.raises(ValueError):
        f()
    names = [e["name"] for e in events(store)]
    assert names.index("run.running") < names.index("run.failed")
    assert "task_run.completed" in names
    failed = [e for e in events(store, "run.failed")][0]
    assert failed["resource"]["kind"] == "run" and failed["related"][0]["kind"] == "flow"
    seqs = [e["seq"] for e in events(store)]
    assert seqs == sorted(seqs)


def test_custom_event_from_task_defaults_to_run(store):
    @task
    def t():
        emit_event("orders.table_empty", {"table": "orders"})

    @flow(tags=["prod"])
    def f():
        t()

    f()
    ev = events(store, "orders.table_empty")[0]
    assert ev["resource"]["kind"] == "run" and ev["payload"] == {"table": "orders"}
    assert {r["kind"] for r in ev["related"]} >= {"flow", "tag", "task_run"}
    emit_event("outside.run", {"x": 1})
    assert events(store, "outside.run")[0]["resource"]["kind"] == "custom"


def test_artifacts_offline(store):
    @task
    def build():
        artifacts.create_table([{"a": 1}, {"a": 2}])
        artifacts.create_progress(10, key="load")
        artifacts.update_progress("load", 60)

    @flow
    def f():
        build()
        artifacts.create_markdown("# hi", key="report")
        artifacts.create_link("https://example.com", "site")
        artifacts.create_image(b"\x89PNG", key="img")

    f()
    run = json.loads(store.list_runs())["items"][0]
    rows = json.loads(store.artifacts(run["id"]))
    kinds = [r["kind"] for r in rows]
    assert sorted(kinds) == ["image", "link", "markdown", "progress", "table"]
    table = next(r for r in rows if r["kind"] == "table")
    assert table["task_run_id"] is not None and len(table["data"]["rows"]) == 2
    progress = next(r for r in rows if r["kind"] == "progress")
    assert progress["data"]["value"] == 60
    assert next(r for r in rows if r["kind"] == "image")["data"]["src"].startswith("data:image/png;base64,")

    @flow
    def big():
        artifacts.create_markdown("x" * 1_100_000)

    with pytest.raises(CereyanError):
        big()
    run = json.loads(store.list_runs())["items"][0]
    assert json.loads(store.artifacts(run["id"])) == []


def test_variables_round_trip_and_secrets(store, isolated_home):
    Variable.set("region", "eu", tags=["infra"])
    assert Variable.get("region") == "eu"
    assert Variable.get("missing", default=3) == 3
    with pytest.raises(ValueError):
        Variable.set("Bad Name", 1)
    with pytest.raises(ValueError):
        Variable.set("toolarge", "x" * 70_000)
    Variable.set("api_token", "hunter2", secret=True)
    listed = {v["name"]: v for v in json.loads(store.list_variables())}
    assert listed["api_token"]["value"] == "********" and listed["api_token"]["secret"]
    assert listed["region"]["value"] == "eu"
    assert Variable.get("api_token") == "hunter2"
    key = isolated_home / "secret.key"
    assert key.exists()
    assert oct(key.stat().st_mode & 0o777) == "0o600"
    key.unlink()
    with pytest.raises(CereyanError) as info:
        Variable.get("api_token")
    assert "unrecoverable" in str(info.value)
    assert Variable.unset("region") is True and Variable.unset("region") is False


def test_code_rule_runs_offline(store):
    calls = []
    app = App("rules")

    @app.rule(on="run.failed")
    def notify(event, run):
        calls.append((event["name"], run["name"]))

    @app.flow
    def boom():
        raise RuntimeError("x")

    from cereyan.rules import register_with_store

    register_with_store(store)
    with pytest.raises(RuntimeError):
        boom()
    assert calls and calls[0][0] == "run.failed"
    rows = json.loads(store.list_rules())
    assert rows[0]["source"] == "code" and rows[0]["do"][0]["kind"] == "call"
    assert rows[0]["fire_count"] == 1
    with pytest.raises(RuntimeError):
        boom()
    assert len(calls) == 2  # once per run, two runs
