import os
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import cereyan
from cereyan import apps as app_module
from cereyan import engine

USER_DB = os.path.join(os.path.expanduser("~"), ".cereyan", "db.sqlite")


def _stat(path):
    try:
        st = os.stat(path)
        return (st.st_mtime_ns, st.st_size)
    except FileNotFoundError:
        return None


@pytest.fixture(scope="session", autouse=True)
def user_home_untouched():
    before = _stat(USER_DB)
    yield
    assert _stat(USER_DB) == before, "the test suite touched ~/.cereyan"


@pytest.fixture(autouse=True)
def isolated_home(tmp_path, monkeypatch):
    home = tmp_path / "home"
    monkeypatch.setenv("CEREYAN_HOME", str(home))
    engine.configure(None)
    engine.close_store()
    app_module.reset_default_app()
    yield home
    engine.close_store()
    app_module.reset_default_app()


@pytest.fixture
def store(isolated_home):
    return engine.get_store()


@pytest.fixture
def run_cli(isolated_home):
    """Run the cereyan CLI in a subprocess against the isolated home."""

    def _run(*args, cwd=None, env=None, home=None):
        full_env = dict(os.environ)
        full_env["CEREYAN_HOME"] = str(home if home is not None else isolated_home)
        if env:
            full_env.update(env)
        return subprocess.run(
            [sys.executable, "-m", "cereyan", *args],
            cwd=cwd,
            env=full_env,
            capture_output=True,
            text=True,
            timeout=120,
        )

    return _run


@pytest.fixture
def example_dir():
    return os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "examples")


@pytest.fixture
def write_module(tmp_path):
    """Write a Python module into a named directory and return its path."""

    def _write(dirname: str, source: str, filename: str = "pipeline.py"):
        directory = tmp_path / dirname
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / filename
        path.write_text(source)
        return path

    return _write


@pytest.fixture
def import_file():
    """Import a module from a file path under a unique module name."""
    import importlib.util

    loaded = []

    def _import(path, name=None):
        name = name or f"testmod_{len(loaded)}_{abs(hash(str(path)))}"
        spec = importlib.util.spec_from_file_location(name, str(path))
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
        loaded.append(name)
        return module

    yield _import
    for name in loaded:
        sys.modules.pop(name, None)


__all__ = ["cereyan"]


@pytest.fixture(scope="session", autouse=True)
def pid_tasks_module(tmp_path_factory):
    """A module-level task file importable by spawned processes."""
    from server_helpers import PIDS_MODULE

    d = tmp_path_factory.mktemp("pidmod")
    (d / "pid_tasks.py").write_text(PIDS_MODULE)
    sys.path.insert(0, str(d))
    yield
    sys.path.remove(str(d))


@pytest.fixture
def project_dir(tmp_path):
    from server_helpers import PIPELINE

    d = tmp_path / "proj"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    return d


@pytest.fixture
def server(isolated_home, project_dir):
    from server_helpers import ServerProcess

    engine.close_store()
    srv = ServerProcess(str(isolated_home), str(project_dir))
    try:
        yield srv
    finally:
        srv.stop()
