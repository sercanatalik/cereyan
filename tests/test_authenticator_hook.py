"""The authenticator hook: enable_auth, credential sources, check order, scope,
startup checks, and serving off the main thread."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

import pytest

from cereyan import serve as serve_mod
from cereyan.client import Client
from cereyan.exceptions import CereyanError
from server_helpers import ServerProcess

AUTH_MODULE = '''
import os
from pipeline import app

USERS = {"good-alice": "alice", "good-bob": "bob"}

@app.authenticator
def check(credential):
    with open(os.path.join(os.environ["CEREYAN_HOME"], "hook-calls.txt"), "a") as fh:
        fh.write(credential + "\\n")
    if credential == "boom":
        raise RuntimeError("hook exploded near secret-detail")
    if credential == "weird":
        return 42
    return USERS.get(credential)
'''

LOGIN = "https://sso.example.com/login"


@pytest.fixture
def auth_dir(project_dir):
    (project_dir / "auth.py").write_text(AUTH_MODULE)
    return project_dir


def start(home, directory, **env) -> ServerProcess:
    from cereyan import engine

    engine.close_store()
    return ServerProcess(str(home), str(directory), env=env)


def request(url, headers=None, method="GET", body=None):
    data = json.dumps(body).encode() if body is not None else None
    headers = dict(headers or {})
    if data is not None:
        headers["content-type"] = "application/json"
    req = urllib.request.Request(url, headers=headers, data=data, method=method)
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status, resp.headers.get("content-type", ""), resp.read().decode()
    except urllib.error.HTTPError as exc:
        return exc.code, exc.headers.get("content-type", ""), exc.read().decode()


def bearer(value):
    return {"authorization": f"Bearer {value}"}


def hook_calls(home) -> list[str]:
    path = os.path.join(str(home), "hook-calls.txt")
    if not os.path.exists(path):
        return []
    with open(path) as fh:
        return fh.read().splitlines()


def serve_fails(home, directory, env) -> str:
    full_env = dict(os.environ)
    full_env.update({"CEREYAN_HOME": str(home), "CEREYAN_NO_BROWSER": "1"})
    full_env.pop("CEREYAN_PORT", None)
    full_env.update(env)
    proc = subprocess.run(
        [sys.executable, "-m", "cereyan", "serve", str(directory), "--port", "0", "--no-open"],
        env=full_env, capture_output=True, text=True, timeout=60,
    )
    assert proc.returncode != 0, proc.stderr
    return proc.stderr


def test_credentials_reach_the_hook_from_the_bearer_header_or_the_cookie(isolated_home, auth_dir):
    (auth_dir / "cereyan.toml").write_text('[server]\nenable_auth = false\nauth_cookie = "FROM_FILE"\n')
    srv = start(isolated_home, auth_dir, CEREYAN_ENABLE_AUTH="true", CEREYAN_AUTH_COOKIE="SSO_SESSION",
                CEREYAN_LOGIN_URL=LOGIN)
    try:
        url = srv.info["url"]
        assert srv.info["auth"] is True and "token" not in srv.info
        status, _, body = request(url + "/api/runs")
        assert status == 401
        assert json.loads(body)["auth"] == "hook" and json.loads(body)["login_url"] == LOGIN
        assert hook_calls(isolated_home) == []
        assert request(url + "/api/runs", bearer("good-alice"))[0] == 200
        assert request(url + "/api/runs", {"cookie": "a=1; SSO_SESSION=good-bob"})[0] == 200
        # The environment outranks cereyan.toml, whose cookie name is ignored.
        assert request(url + "/api/runs", {"cookie": "FROM_FILE=good-bob"})[0] == 401
        request(url + "/api/runs", {**bearer("good-alice"), "cookie": "SSO_SESSION=good-bob"})
        assert hook_calls(isolated_home)[-1] == "good-alice"
        status, _, body = request(url + "/api/runs", bearer("nobody"))
        assert status == 401 and json.loads(body)["error"] == "the credential was rejected"
        status, _, body = request(url + "/api/runs", bearer("boom"))
        assert status == 401 and "secret-detail" not in body
        assert request(url + "/api/runs", bearer("weird"))[0] == 401
        assert request(url + "/api/health")[0] == 200
        assert request(url + "/")[0] == 200
        log = srv.read_log()
        assert "hook exploded near secret-detail" in log
        assert "auth enabled" in log
    finally:
        srv.stop()


def test_runs_record_the_user_and_engines_never_reach_the_hook(isolated_home, auth_dir):
    srv = start(isolated_home, auth_dir, CEREYAN_ENABLE_AUTH="1")
    try:
        url = srv.info["url"]
        client = Client(url, token="good-alice")
        srv.client = client
        fid = next(f["id"] for f in client.flows() if f["name"] == "etl")
        run = client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": {"day": "2026-09-06"}})
        assert run["created_by"] == "user:alice"
        assert srv.wait_run(run["id"])["state"]["type"] == "Completed"
        spoofed = client._request("POST", "/api/runs", body={
            "project": "proj", "flow": "etl", "parameters": {"day": "2026-09-07"}, "created_by": "schedule:1",
        })
        assert spoofed["created_by"] == "user:alice"
        assert srv.wait_run(spoofed["id"])["state"]["type"] == "Completed"
        status, _, body = request(url + "/mcp", bearer("good-bob"), "POST", {
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "run_flow", "arguments": {"flow": "etl", "project": "proj",
                                                         "parameters": {"day": "2026-09-08"}}},
        })
        assert status == 200
        result = json.loads(body)["result"]
        assert result["isError"] is False
        assert json.loads(result["content"][0]["text"])["run"]["created_by"] == "user:bob"
        # Engines carry the generated token, which the check accepts before the hook.
        assert set(hook_calls(isolated_home)) == {"good-alice", "good-bob"}
    finally:
        srv.stop()


def test_a_registered_authenticator_is_not_called_while_auth_is_disabled(isolated_home, auth_dir):
    srv = start(isolated_home, auth_dir, CEREYAN_AUTH_COOKIE="SSO_SESSION", CEREYAN_HOST="0.0.0.0")
    try:
        assert request(srv.info["url"] + "/api/runs")[0] == 200
        assert hook_calls(isolated_home) == []
        log = srv.read_log()
        assert "registered but disabled" in log
        assert "auth_cookie has no effect" in log
        assert "unauthenticated" in log
    finally:
        srv.stop()


def test_scope_all_covers_the_ui_and_custom_routes(isolated_home, auth_dir):
    srv = start(isolated_home, auth_dir, CEREYAN_ENABLE_AUTH="yes", CEREYAN_AUTH_COOKIE="SSO_SESSION",
                CEREYAN_AUTH_SCOPE="all", CEREYAN_LOGIN_URL=LOGIN + "?next=/runs&x=1")
    try:
        url = srv.info["url"]
        status, content_type, body = request(url + "/runs/42", {"accept": "text/html,application/xhtml+xml"})
        assert status == 401 and content_type.startswith("text/html")
        assert f'href="{LOGIN}?next=/runs&amp;x=1"' in body
        status, _, body = request(url + "/health")
        assert status == 401 and json.loads(body)["auth"] == "hook"
        assert request(url + "/api/nope")[0] == 401
        cookie = {"cookie": "SSO_SESSION=good-alice"}
        assert request(url + "/", cookie)[0] == 200
        assert request(url + "/health", cookie)[0] == 200
        assert request(url + "/api/nope", cookie)[0] == 404
        assert request(url + "/api/health")[0] == 200
    finally:
        srv.stop()


@pytest.mark.parametrize(
    ("env", "needle"),
    [
        ({"CEREYAN_ENABLE_AUTH": "maybe"}, "CEREYAN_ENABLE_AUTH"),
        ({"CEREYAN_AUTH_COOKIE": "cereyan_token"}, "auth_cookie"),
        ({"CEREYAN_AUTH_SCOPE": "everything"}, "auth_scope"),
        ({"CEREYAN_AUTH_SCOPE": "all"}, "auth_scope"),
        ({"CEREYAN_ENABLE_AUTH": "true", "CEREYAN_LOGIN_URL": "javascript:alert(1)"}, "login_url"),
    ],
)
def test_invalid_auth_settings_stop_the_server(isolated_home, auth_dir, env, needle):
    assert needle in serve_fails(isolated_home, auth_dir, env)


def test_enable_auth_without_an_authenticator_fails_closed(isolated_home, project_dir):
    (project_dir / "broken_auth.py").write_text("from pipeline import app\nraise ImportError('no jwt library')\n")
    err = serve_fails(isolated_home, project_dir, {"CEREYAN_ENABLE_AUTH": "true"})
    assert "enable_auth" in err and "broken_auth" in err


def test_two_authenticators_name_both(isolated_home, auth_dir):
    (auth_dir / "second.py").write_text(
        "from pipeline import app\n\n@app.authenticator\ndef other(credential):\n    return None\n"
    )
    err = serve_fails(isolated_home, auth_dir, {})
    assert "auth.check" in err and "second.other" in err


def test_enable_auth_parsing_and_precedence(tmp_path, monkeypatch):
    monkeypatch.delenv("CEREYAN_ENABLE_AUTH", raising=False)
    # cereyan.toml is cached per directory, so each case gets its own.
    bare, configured = tmp_path / "bare", tmp_path / "configured"
    bare.mkdir()
    configured.mkdir()
    (configured / "cereyan.toml").write_text("[server]\nenable_auth = true\n")
    assert serve_mod.resolve_enable_auth(str(bare)) is False
    directory = str(configured)
    assert serve_mod.resolve_enable_auth(directory) is True
    assert serve_mod.resolve_enable_auth(directory, app_enable_auth=False) is False
    for value, expected in (("TRUE", True), ("yes", True), ("1", True), ("False", False), ("no", False), ("0", False)):
        monkeypatch.setenv("CEREYAN_ENABLE_AUTH", value)
        assert serve_mod.resolve_enable_auth(directory, app_enable_auth=not expected) is expected
    assert serve_mod.resolve_enable_auth(directory, enable_auth=True) is True
    monkeypatch.setenv("CEREYAN_ENABLE_AUTH", "maybe")
    with pytest.raises(CereyanError, match="CEREYAN_ENABLE_AUTH"):
        serve_mod.resolve_enable_auth(directory)


def test_login_url_scope_and_cookie_validation(tmp_path, monkeypatch):
    for name in ("CEREYAN_LOGIN_URL", "CEREYAN_AUTH_COOKIE", "CEREYAN_AUTH_SCOPE"):
        monkeypatch.delenv(name, raising=False)
    directory = str(tmp_path)
    for ok in ("https://sso.example.com/login", "http://sso:8080", "/login"):
        assert serve_mod.resolve_login_url(directory, ok) == ok
    for bad in ("javascript:alert(1)", "//evil.example.com", "ftp://x", "login", "https://"):
        with pytest.raises(CereyanError, match="--login-url"):
            serve_mod.resolve_login_url(directory, bad)
    assert serve_mod.resolve_auth_scope(directory) == "api"
    with pytest.raises(CereyanError, match="auth_scope"):
        serve_mod.resolve_auth_scope(directory, app_auth_scope="everything")
    assert serve_mod.resolve_auth_cookie(directory, app_auth_cookie="SSO_SESSION") == "SSO_SESSION"
    with pytest.raises(CereyanError, match="cereyan_token"):
        serve_mod.resolve_auth_cookie(directory, "cereyan_token")


def test_app_serve_runs_on_a_background_thread(isolated_home, monkeypatch):
    from cereyan import App, engine

    monkeypatch.setenv("CEREYAN_NO_BROWSER", "1")
    engine.close_store()
    app = App("threaded")
    started: list = []
    errors: list = []

    def run():
        try:
            app.serve(port=0, ready=started.append, quiet=True)
        except BaseException as exc:  # noqa: BLE001 - reported below
            errors.append(exc)

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    deadline = time.time() + 30
    while not started and not errors and time.time() < deadline:
        time.sleep(0.05)
    assert not errors, errors
    assert started, "the server never became ready"
    server = started[0]
    discovery = isolated_home / "server.json"
    assert discovery.exists()
    with urllib.request.urlopen(server.url + "/api/health", timeout=5) as resp:
        assert resp.status == 200
    server.stop()
    thread.join(timeout=30)
    assert not thread.is_alive()
    assert not errors, errors
    assert not discovery.exists()
