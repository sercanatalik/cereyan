import json

import pytest

import cereyan
from cereyan import App, _core
from cereyan.exceptions import FlowRegistrationError

MODULE = '''
from cereyan import flow, task

@task
def step():
    return 1

@flow
def daily_etl():
    return step()
'''


def test_explicit_app_name(store):
    app = App("warehouse")

    @app.flow
    def f():
        pass

    f()
    flows = json.loads(store.list_flows())
    assert [(x["project"], x["name"]) for x in flows] == [("warehouse", "f")]


@pytest.mark.parametrize("bad", ["My Project", "Warehouse", "-x", "", "a b"])
def test_invalid_app_name(bad):
    with pytest.raises(ValueError) as info:
        App(bad)
    assert "[a-z0-9]" in str(info.value)


def test_default_name_from_module_directory(write_module, import_file, monkeypatch, tmp_path, store):
    path = write_module("etl", MODULE)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)
    module = import_file(path)
    assert module.daily_etl.project == "etl"
    module.daily_etl()
    flows = json.loads(store.list_flows())
    assert flows[0]["project"] == "etl"
    assert flows[0]["source_dir"] == str(path.parent)
    assert flows[0]["module"] == module.__name__


def test_same_name_in_two_projects_are_distinct(store):
    a = App("a")
    b = App("b")

    @a.flow(name="daily_etl")
    def fa():
        pass

    @b.flow(name="daily_etl")
    def fb():
        pass

    fa()
    fb()
    flows = json.loads(store.list_flows())
    assert sorted((f["project"], f["name"]) for f in flows) == [("a", "daily_etl"), ("b", "daily_etl")]
    runs_a = json.loads(store.list_runs(json.dumps({"project": "a"})))["items"]
    runs_b = json.loads(store.list_runs(json.dumps({"project": "b"})))["items"]
    assert len(runs_a) == 1 and len(runs_b) == 1
    assert runs_a[0]["flow_id"] != runs_b[0]["flow_id"]


def test_duplicate_within_project():
    app = App("p")

    @app.flow(name="load")
    def one():
        pass

    with pytest.raises(FlowRegistrationError):
        @app.flow(name="load")
        def two():
            pass


def test_registration_upserts_and_preserves_history(store):
    app = App("p")

    @app.flow(name="f", tags=["x"])
    def f():
        pass

    f()
    first = json.loads(store.list_flows())[0]
    # Re-register with new metadata from a "new" process: same id, updated fields.
    flow_id = store.upsert_flow("p", "f", "other.module", "/new/dir", "changed", '["y"]', "{}")
    assert flow_id == first["id"]
    after = json.loads(store.list_flows())[0]
    assert after["source_dir"] == "/new/dir"
    assert after["tags"] == ["y"]
    assert after["last_seen_at"] >= first["last_seen_at"]
    assert len(json.loads(store.list_runs())["items"]) == 1


def test_project_filter_and_run_carries_project(store):
    app = App("warehouse")

    @app.flow
    def f():
        pass

    f()
    runs = json.loads(store.list_runs(json.dumps({"project": "warehouse"})))["items"]
    assert runs[0]["project"] == "warehouse"
    assert json.loads(store.list_runs(json.dumps({"project": "nope"})))["items"] == []


def test_the_four_version_strings_agree():
    """The release checklist bumps four files by hand; nothing else catches a missed one."""
    import pathlib, tomllib

    root = pathlib.Path(__file__).resolve().parent.parent
    if not (root / "pyproject.toml").exists():  # installed wheel, not a source checkout
        pytest.skip("not a source checkout")
    versions = {
        "pyproject.toml": tomllib.loads((root / "pyproject.toml").read_text())["project"]["version"],
        "Cargo.toml": tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"],
        "ui/package.json": json.loads((root / "ui" / "package.json").read_text())["version"],
        "cereyan.__version__": cereyan.__version__,
        "cereyan._core.__version__": _core.__version__,
    }
    assert len(set(versions.values())) == 1, versions


def test_flow_group_declared_and_defaulted(store):
    app = App("warehouse")

    @app.flow(group="nightly")
    def load():
        pass

    @app.flow
    def reconcile():
        pass

    assert load.group == "nightly" and load.declared_group == "nightly"
    # No group of its own: the flow reads as grouped under its project.
    assert reconcile.group == "warehouse" and reconcile.declared_group is None
    # It is flow metadata, not an execution option.
    assert "group" not in load.options and "group" not in reconcile.options

    load()
    reconcile()
    flows = {f["name"]: f for f in json.loads(store.list_flows())}
    assert flows["load"]["group"] == "nightly"
    assert flows["reconcile"]["group"] is None
    runs = {r["flow_name"]: r for r in json.loads(store.list_runs())["items"]}
    assert runs["load"]["group"] == "nightly"
    assert runs["reconcile"]["group"] == "warehouse"


def test_flow_group_spans_projects(store):
    warehouse, analytics = App("warehouse"), App("analytics")

    @warehouse.flow(group="nightly")
    def load():
        pass

    @analytics.flow(group="nightly")
    def rollup():
        pass

    load()
    rollup()
    flows = json.loads(store.list_flows())
    assert {(f["project"], f["group"]) for f in flows} == {
        ("warehouse", "nightly"),
        ("analytics", "nightly"),
    }


def test_flow_group_follows_a_rename(store):
    app = App("warehouse")

    @app.flow(group="nightly")
    def load():
        pass

    load()
    run_id = json.loads(store.list_runs())["items"][0]["id"]
    assert json.loads(store.get_run(run_id))["group"] == "nightly"

    # Re-registering with a new group moves the history: nothing is stored on the run.
    app2 = App("warehouse")

    @app2.flow(name="load", group="overnight")
    def load2():
        pass

    load2()
    assert json.loads(store.get_run(run_id))["group"] == "overnight"
