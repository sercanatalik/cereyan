"""Fixtures for the code blocks under ``docs/`` and the files under ``examples/``.

``docs/conftest.py`` and ``tests/docs/conftest.py`` both import this module, so the
same fixtures serve the pages (collected by pytest-markdown-docs) and the example
files (run by ``test_examples.py``).

- Every block runs with ``CEREYAN_HOME`` pointed at one temporary home for the
  session and the working directory set to a fresh temporary directory, so no
  block touches ``~/.cereyan`` or the repository.
- A block that needs a server names the ``served`` fixture on its fence
  (```` ```{.python fixture:served} ````). One server process is started for the
  session on its own home, serving ``examples/``; the block sees it as ``served``
  with ``served.url``, ``served.client``, and ``served.wait_run(run_id)``.
"""

from __future__ import annotations

import os
import sys

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TESTS = os.path.join(ROOT, "tests")
EXAMPLES = os.path.join(ROOT, "examples")
if TESTS not in sys.path:
    sys.path.insert(0, TESTS)

from cereyan import apps as app_module  # noqa: E402
from cereyan import engine  # noqa: E402
from cereyan import rules as rules_module  # noqa: E402

USER_DB = os.path.join(os.path.expanduser("~"), ".cereyan", "db.sqlite")


def _stat(path):
    try:
        st = os.stat(path)
        return (st.st_mtime_ns, st.st_size)
    except FileNotFoundError:
        return None


@pytest.fixture(scope="session", autouse=True)
def docs_user_home_untouched():
    before = _stat(USER_DB)
    yield
    assert _stat(USER_DB) == before, "the documentation tests touched ~/.cereyan"


@pytest.fixture(scope="session")
def docs_home(tmp_path_factory):
    """One runtime home for every offline block in the session."""
    return str(tmp_path_factory.mktemp("docs-home"))


@pytest.fixture(autouse=True)
def docs_block(request, docs_home, tmp_path, monkeypatch):
    """Isolate each block: temporary home, temporary working directory, fresh default App.

    A block that names the ``served`` fixture gets the session server's home instead,
    so ``cereyan.client`` and offline handoff find the server through ``server.json``.
    """
    home = docs_home
    if "served" in request.fixturenames:
        home = str(request.getfixturevalue("docs_server").home)
    monkeypatch.setenv("CEREYAN_HOME", home)
    monkeypatch.setenv("CEREYAN_NO_BROWSER", "1")
    for var in ("CEREYAN_TOKEN", "CEREYAN_SOCKET", "CEREYAN_HOST", "CEREYAN_PORT"):
        monkeypatch.delenv(var, raising=False)
    monkeypatch.chdir(tmp_path)
    engine.configure(None)
    engine.close_store()
    app_module.reset_default_app()
    # Code rules registered by a block or an example must not leak into later tests.
    saved = [dict(rules_module._registry), dict(rules_module._guards), dict(rules_module._expectations)]
    yield
    engine.close_store()
    app_module.reset_default_app()
    for live, before in zip((rules_module._registry, rules_module._guards, rules_module._expectations), saved):
        live.clear()
        live.update(before)


@pytest.fixture(scope="session")
def docs_server(tmp_path_factory):
    """One ``cereyan serve examples/`` process for the session, on its own home."""
    from server_helpers import ServerProcess

    home = tmp_path_factory.mktemp("docs-served-home")
    engine.close_store()
    server = ServerProcess(str(home), EXAMPLES)
    server.url = server.info["url"]
    try:
        yield server
    finally:
        server.stop()


@pytest.fixture
def served(docs_server):
    """The session server, exposed to the block as ``served`` (see ``docs_block`` for the home)."""
    return docs_server
