import json
from datetime import date

import pytest

import cereyan
from cereyan import App, flow, task
from cereyan.exceptions import FlowRegistrationError


def test_decorated_function_still_callable(store):
    @flow
    def etl(day: date) -> int:
        return day.year

    assert etl(date(2026, 1, 1)) == 2026
    assert etl.__name__ == "etl"


def test_flow_registered_on_default_app():
    @flow
    def my_flow():
        pass

    assert "my_flow" in cereyan.app.flows
    assert cereyan.app.flows["my_flow"] is my_flow


def test_app_flow_decorator_equivalent():
    app = App("warehouse")

    @app.flow
    def load():
        pass

    assert app.flows["load"] is load
    assert load.project == "warehouse"


def test_duplicate_flow_name_names_both_locations():
    app = App("proj")

    @app.flow(name="dup")
    def one():
        pass

    with pytest.raises(FlowRegistrationError) as info:
        @app.flow(name="dup")
        def two():
            pass

    text = str(info.value)
    assert "dup" in text
    assert text.count(__file__) == 2


def test_flow_metadata_and_templated_run_name(store):
    @flow(name="named", description="desc", tags=["b", "a"], run_name="etl-{day}")
    def f(day: date):
        return 1

    assert f.name == "named"
    assert f.description == "desc"
    assert f.tags == ["a", "b"]
    f(day=date(2026, 9, 6))
    runs = json.loads(store.list_runs())["items"]
    assert runs[0]["name"] == "etl-2026-09-06"
    assert runs[0]["tags"] == ["a", "b"]


def test_description_defaults_to_docstring():
    @flow
    def f():
        """Loads things."""

    assert f.description == "Loads things."


def test_default_run_name_is_two_words_and_unique(store):
    @flow
    def f():
        pass

    names = set()
    for _ in range(5):
        f()
        names.add(cereyan.engine.get_store().list_runs())
    runs = json.loads(store.list_runs())["items"]
    generated = [r["name"] for r in runs]
    assert len(set(generated)) == 5
    for name in generated:
        parts = name.split("-")
        assert len(parts) >= 2 and all(parts)


def test_runtime_context_inside_task_and_outside(store):
    seen = {}

    @task
    def probe():
        seen["run_id"] = cereyan.runtime.run.id
        seen["run_name"] = cereyan.runtime.run.name
        seen["flow_name"] = cereyan.runtime.flow.name
        seen["params"] = cereyan.runtime.run.parameters
        seen["task_name"] = cereyan.runtime.task_run.name
        seen["task_run_id"] = cereyan.runtime.task_run.id

    @flow
    def f(x: int = 3):
        probe()

    assert cereyan.runtime.run is None
    assert cereyan.runtime.task_run is None
    f()
    run = json.loads(store.list_runs())["items"][0]
    assert seen["run_id"] == run["id"]
    assert seen["run_name"] == run["name"]
    assert seen["flow_name"] == "f"
    assert seen["params"] == {"x": 3}
    assert seen["task_name"] == "probe"
    tasks = json.loads(store.task_runs(run["id"]))
    assert tasks[0]["external_id"] == seen["task_run_id"]
    assert cereyan.runtime.run is None


def test_same_task_called_twice_gets_dynamic_keys(store):
    @task
    def load():
        return 1

    @flow
    def f():
        load()
        load()

    f()
    run = json.loads(store.list_runs())["items"][0]
    tasks = json.loads(store.task_runs(run["id"]))
    assert [t["dynamic_key"] for t in tasks] == ["load-0", "load-1"]
    assert all(t["task_key"].endswith("load") for t in tasks)
    assert all(t["state"]["type"] == "Completed" for t in tasks)


def test_task_outside_flow_is_plain_call(store):
    @task
    def add(a, b):
        return a + b

    assert add(1, 2) == 3
    assert json.loads(store.list_runs())["items"] == []
