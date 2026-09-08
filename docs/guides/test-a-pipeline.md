# How to test a pipeline

Flows are functions and every run is recorded in whatever home you point cereyan at, so tests need no server and no mocks: give each test a temporary home, call the flow, and assert on the return value, the files it wrote, or the recorded run.

## Isolate the home

Set `CEREYAN_HOME` to a temporary directory before the flow runs, so tests never touch `~/.cereyan`:

<!-- notest: pytest fixture definitions, not a runnable block -->
```{.python notest}
# conftest.py
import pytest

@pytest.fixture(autouse=True)
def cereyan_home(tmp_path, monkeypatch):
    monkeypatch.setenv("CEREYAN_HOME", str(tmp_path / "home"))
    monkeypatch.chdir(tmp_path)
```

## Call the flow

```python
from datetime import date
from cereyan import flow, task, LocalTarget

@task(output=lambda day: LocalTarget(f"out/{day}.csv"))
def build(day: date) -> None:
    with LocalTarget(f"out/{day}.csv").open("w") as fh:
        fh.write("id\n1\n")

@flow
def daily(day: date) -> str:
    build(day)
    return f"out/{day}.csv"

def test_daily_writes_the_file():
    path = daily(date(2026, 1, 1))
    assert LocalTarget(path).exists()

test_daily_writes_the_file()
```

A flow that raises propagates the exception after recording the run as Failed, so `pytest.raises` works as usual. Parameters are coerced on the way in, so passing strings tests the same path the CLI and the API use.

## Assert on the recorded run

`cereyan runs ls --json` reads the home's store, so a test that drives the CLI can check what was recorded. Run the flow in a subprocess too, since a process that is executing a flow holds the store lock:

```python
import json, subprocess, sys, textwrap

open("pipeline.py", "w").write(textwrap.dedent("""
    from cereyan import flow

    @flow(tags=["nightly"])
    def tagged() -> int:
        return 1
"""))

subprocess.run([sys.executable, "-m", "cereyan", "run", "pipeline.py:tagged", "--quiet"], check=True)
out = subprocess.run([sys.executable, "-m", "cereyan", "runs", "ls", "--flow", "tagged", "--json"], capture_output=True, text=True, check=True)
runs = json.loads(out.stdout)
assert runs[0]["state"]["type"] == "Completed" and "nightly" in runs[0]["tags"]
```

## Test hooks and rules without a server

Hooks are plain functions; call the flow and check what they captured. Code rules do not fire offline until a server has registered them, so test the rule's function directly with a fake event and run:

```python
from cereyan import App

app = App("tests")
alerts = []

@app.rule(on="run.failed", flow="nightly")
def page(event, run):
    alerts.append(run["name"])

page({"name": "run.failed"}, {"name": "nightly-1"})
assert alerts == ["nightly-1"]
assert app.rules[0].spec()["when"]["events"] == ["run.failed"]
```

## Test against a server

For schedules, backfills, dependencies, rules, routes, and pauses, start a server on a temporary home in a session fixture and drive it through the client. The pattern used by cereyan's own suite and its documentation tests is in `tests/server_helpers.py`: start `cereyan serve <dir> --port 0 --no-open` with `CEREYAN_HOME` set, wait for `server.json` and `/api/health`, and stop it with SIGTERM.

```{.python fixture:served}
run = served.client.run("etl", day="2026-04-01")
final = served.wait_run(run["id"])
assert final["state"]["type"] == "Completed"
assert any(t["name"] == "load" for t in served.client.task_runs(run["id"]))
```

## Speed

Offline runs cost a few milliseconds each. Keep the home per test (a fresh SQLite file is cheap) rather than per session when tests assert on run lists, so counts do not leak between tests.

Related: [Engines and the home directory](../concepts/engines-and-home.md), [Run code on state changes](state-hooks.md).
