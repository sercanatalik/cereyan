"""Flows in sub-folders of the served directory: `source_dir` is the import root,
so server engines, workers, the supervisor and MCP all find the same module."""

from __future__ import annotations

import importlib
import importlib.util
import os
import subprocess
import sys
import time

import pytest

from cereyan.client import Client
from server_helpers import ServerProcess

NESTED = '''
import os
from cereyan import App

app = App("nested")

@app.flow
def financing():
    return os.getpid()
'''


def layout(root, *, packages=False, filename="app.py", source=NESTED):
    """Write `root/flows/financing/<filename>`, with `__init__.py` files when `packages`."""
    leaf = root / "flows" / "financing"
    leaf.mkdir(parents=True)
    if packages:
        (root / "flows" / "__init__.py").write_text("")
        if filename != "__init__.py":
            (leaf / "__init__.py").write_text("")
    (leaf / filename).write_text(source)
    return leaf


@pytest.fixture
def importable(tmp_path):
    """Import a dotted module with `tmp_path / "root"` on sys.path; undo both afterwards."""
    root = tmp_path / "root"
    root.mkdir()
    sys.path.insert(0, str(root))
    importlib.invalidate_caches()

    def _import(name):
        return importlib.import_module(name)

    yield root, _import
    sys.path.remove(str(root))
    for name in [m for m in sys.modules if m == "flows" or m.startswith("flows.")]:
        del sys.modules[name]


def the_flow(module):
    return module.app.flows["financing"]


@pytest.mark.parametrize("packages", [False, True], ids=["namespace", "init"])
def test_nested_flow_records_the_import_root(importable, packages):
    root, imp = importable
    leaf = layout(root, packages=packages)
    f = the_flow(imp("flows.financing.app"))
    assert f.module == "flows.financing.app"
    assert f.source_file == str(leaf / "app.py")
    assert f.source_dir == str(root)


def test_flow_in_a_package_init_records_the_import_root(importable):
    root, imp = importable
    layout(root, packages=True, filename="__init__.py")
    f = the_flow(imp("flows.financing"))
    assert f.module == "flows.financing"
    assert f.source_dir == str(root)


def test_flat_flow_keeps_its_folder(importable):
    root, imp = importable
    (root / "flat_pipeline.py").write_text(NESTED)
    try:
        f = the_flow(imp("flat_pipeline"))
        assert f.source_dir == str(root)
    finally:
        sys.modules.pop("flat_pipeline", None)


def test_a_name_that_does_not_match_the_path_keeps_the_folder(tmp_path):
    leaf = layout(tmp_path / "root")
    spec = importlib.util.spec_from_file_location("elsewhere.named.app", str(leaf / "app.py"))
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
        f = the_flow(module)
        assert f.module == "elsewhere.named.app"
        assert f.source_dir == str(leaf)
    finally:
        sys.modules.pop(spec.name, None)


def test_script_run_directly_keeps_its_folder(tmp_path):
    leaf = layout(tmp_path / "root", source=NESTED + "\nprint(financing.module, financing.source_dir)\n")
    out = subprocess.run([sys.executable, str(leaf / "app.py")], capture_output=True, text=True,
                         env=dict(os.environ, CEREYAN_HOME=str(tmp_path / "home")), timeout=60)
    assert out.returncode == 0, out.stderr
    module, source_dir = out.stdout.split()
    assert module == "app"
    assert source_dir == str(leaf)


@pytest.fixture
def nested_server(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    root = tmp_path / "proj"
    root.mkdir()
    layout(root)
    srv = ServerProcess(str(isolated_home), str(root), max_engines=1)
    srv.root = root
    try:
        yield srv
    finally:
        srv.stop()


def start(srv):
    fid = next(f["id"] for f in srv.client.flows() if f["name"] == "financing")
    return srv.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": {}})


def test_nested_flow_runs_on_a_server_engine(nested_server):
    flow = next(f for f in nested_server.client.flows() if f["name"] == "financing")
    assert flow["module"] == "flows.financing.app"
    done = nested_server.wait_run(start(nested_server)["id"])
    assert done["state"]["type"] == "Completed", nested_server.read_log()


def test_editing_a_nested_flow_recycles_its_engine(nested_server):
    first = nested_server.wait_run(start(nested_server)["id"])
    assert first["state"]["type"] == "Completed", nested_server.read_log()
    time.sleep(1.1)
    path = nested_server.root / "flows" / "financing" / "app.py"
    path.write_text(path.read_text().replace("return os.getpid()", "raise ValueError('edited')"))
    os.utime(path, None)
    second = nested_server.wait_run(start(nested_server)["id"])
    assert second["engine_pid"] != first["engine_pid"]
    assert second["state"]["type"] == "Failed"
    assert "edited" in (second["state"].get("message") or ""), second["state"]


def test_mcp_reads_a_nested_flows_source(isolated_home, tmp_path):
    from cereyan import engine
    from test_agent_mcp import TOKEN, call, rpc

    engine.close_store()
    root = tmp_path / "proj"
    root.mkdir()
    layout(root)
    srv = ServerProcess(str(isolated_home), str(root), env={"CEREYAN_TOKEN": TOKEN})
    srv.client = Client(srv.info["url"], token=TOKEN)
    srv.session = None
    srv.counter = 0
    try:
        rpc(srv, "initialize", {"clientInfo": {"name": "reader"}})
        source = call(srv, "get_flow_source", flow="financing")
        assert source["isError"] is False, source
        assert "def financing(" in source["data"]["source"]
        assert source["data"]["path"].endswith(os.path.join("flows", "financing", "app.py"))
    finally:
        srv.stop()


# ---------------------------------------------------------------- entry points

COMMON = '''
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def settings():
    return json.loads((ROOT / "config" / "settings.json").read_text())
'''

USES_ROOT = '''
from cereyan import App
from flows.common.config import settings

app = App("nested")

@app.flow
def financing():
    return settings()["owner"]
'''

ONEOFF = '''
import sys
from cereyan import flow
from flows.common.config import settings

@flow
def oneoff(tag: str = "x") -> str:
    return tag + ":" + settings()["owner"]

if __name__ == "__main__":
    if sys.argv[1:] == ["describe"]:
        print(oneoff.module, oneoff.source_dir)
    else:
        oneoff(tag="script")
'''

RISK_INIT = '''
from cereyan import App

app = App("nested")

@app.flow
def risk_heartbeat():
    return __name__
'''


def project(root):
    """A nested project whose modules import a helper by its path from `root`."""
    layout(root, source=USES_ROOT)
    (root / "flows" / "common").mkdir()
    (root / "flows" / "common" / "config.py").write_text(COMMON)
    (root / "flows" / "financing" / "oneoff.py").write_text(ONEOFF)
    (root / "config").mkdir()
    (root / "config" / "settings.json").write_text('{"owner": "finance-data"}')
    return root


@pytest.fixture
def in_project(tmp_path, monkeypatch):
    """`tmp_path / "proj"` as the current directory; undo sys.path and sys.modules afterwards."""
    root = project(tmp_path / "proj")
    monkeypatch.chdir(root)
    before = list(sys.path)
    yield root
    sys.path[:] = before
    for name in [m for m in sys.modules if m == "flows" or m.startswith("flows.") or m == "app"]:
        del sys.modules[name]


def test_run_a_nested_file_by_path(in_project, run_cli):
    from cereyan.cli import load_target

    flow = load_target("flows/financing/app.py:financing")
    assert flow.module == "flows.financing.app"
    assert flow.source_dir == str(in_project)
    out = run_cli("run", "flows/financing/app.py:financing", cwd=str(in_project))
    assert out.returncode == 0, out.stdout + out.stderr


def test_run_a_flat_file_by_path_keeps_its_name(tmp_path, monkeypatch):
    from cereyan.cli import load_target

    (tmp_path / "flat_target.py").write_text(NESTED)
    monkeypatch.chdir(tmp_path)
    before = list(sys.path)
    try:
        flow = load_target("flat_target.py:financing")
        assert flow.module == "flat_target" and flow.source_dir == str(tmp_path)
    finally:
        sys.path[:] = before
        sys.modules.pop("flat_target", None)


def test_run_refuses_a_dotted_path_bound_to_another_file(in_project, tmp_path):
    from cereyan.cli import load_target
    from cereyan.exceptions import CereyanError

    other = tmp_path / "other"
    layout(other)
    sys.path.insert(0, str(other))
    importlib.import_module("flows.financing.app")  # binds the name to other/
    with pytest.raises(CereyanError) as err:
        load_target("flows/financing/app.py:financing")
    assert str(other / "flows" / "financing" / "app.py") in str(err.value)
    assert str(in_project / "flows" / "financing" / "app.py") in str(err.value)


def test_python_dash_m_records_the_dotted_module(tmp_path):
    root = project(tmp_path / "proj")
    out = subprocess.run([sys.executable, "-m", "flows.financing.oneoff", "describe"], cwd=str(root),
                         capture_output=True, text=True, env=dict(os.environ, CEREYAN_HOME=str(tmp_path / "home")),
                         timeout=60)
    assert out.returncode == 0, out.stderr
    module, source_dir = out.stdout.split()
    assert module == "flows.financing.oneoff"
    assert source_dir == str(root)


def test_python_dash_m_hands_off_to_a_server_started_elsewhere(isolated_home, tmp_path):
    from cereyan import engine

    root = project(tmp_path / "proj")
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    engine.close_store()
    # The server's working directory is not the project root, so only a correct
    # source_dir lets its engine import `flows.common.config`.
    srv = ServerProcess(str(isolated_home), str(elsewhere), max_engines=1)
    try:
        out = subprocess.run([sys.executable, "-m", "flows.financing.oneoff"], cwd=str(root), capture_output=True,
                             text=True, env=dict(os.environ, CEREYAN_HOME=str(isolated_home)), timeout=120)
        assert out.returncode == 0, out.stdout + out.stderr + srv.read_log()
        flow = next(f for f in srv.client.flows() if f["name"] == "oneoff")
        assert flow["module"] == "flows.financing.oneoff" and flow["source_dir"] == str(root)
        run = srv.client.runs(limit=5)["items"][0]
        assert run["state"]["type"] == "Completed", run["state"]
    finally:
        srv.stop()


def test_mcp_reads_a_flow_defined_in_a_package_init(isolated_home, tmp_path):
    from cereyan import engine
    from test_agent_mcp import TOKEN, call, rpc

    engine.close_store()
    root = tmp_path / "proj"
    root.mkdir()
    layout(root, packages=True)
    (root / "flows" / "risk").mkdir()
    (root / "flows" / "risk" / "__init__.py").write_text(RISK_INIT)
    (root / "flows" / "risk" / "check.py").write_text("")  # discovery imports the package through it
    srv = ServerProcess(str(isolated_home), str(root), env={"CEREYAN_TOKEN": TOKEN})
    srv.client = Client(srv.info["url"], token=TOKEN)
    srv.session = None
    srv.counter = 0
    try:
        rpc(srv, "initialize", {"clientInfo": {"name": "reader"}})
        source = call(srv, "get_flow_source", flow="risk_heartbeat")
        assert source["isError"] is False, source
        assert source["data"]["module"] == "flows.risk"
        assert source["data"]["path"].endswith(os.path.join("flows", "risk", "__init__.py"))
    finally:
        srv.stop()
