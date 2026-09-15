"""Where each setting comes from, as serve.py records it for the Environment tab."""

from __future__ import annotations

import pytest

from cereyan import serve as serve_mod

ENV = (
    "CEREYAN_HOST", "CEREYAN_PORT", "CEREYAN_TOKEN", "CEREYAN_SOCKET", "CEREYAN_BASE_PATH",
    "CEREYAN_ENABLE_AUTH", "CEREYAN_AUTH_COOKIE", "CEREYAN_AUTH_SCOPE", "CEREYAN_LOGIN_URL",
)


@pytest.fixture(autouse=True)
def clean_env(monkeypatch):
    for name in ENV:
        monkeypatch.delenv(name, raising=False)


def project(tmp_path, name: str, toml: str | None = None) -> str:
    # Every case gets its own directory: cereyan.toml is cached per directory.
    directory = tmp_path / name
    directory.mkdir()
    if toml is not None:
        (directory / "cereyan.toml").write_text(toml)
    return str(directory)


def src(kind: str, name: str | None = None) -> dict:
    return {"source": kind, "name": name}


def test_port_from_each_source(tmp_path, monkeypatch):
    bare = project(tmp_path, "bare")
    host, port, host_source, port_source = serve_mod._host_port(bare, None, None)
    assert (host, port, host_source, port_source) == ("127.0.0.1", 4200, src("default"), src("default"))

    filed = project(tmp_path, "filed", "[server]\nport = 4300\n")
    assert serve_mod._host_port(filed, None, None)[1::2] == (4300, src("toml", "[server] port"))
    assert serve_mod._host_port(filed, None, None, app_port=4400)[1::2] == (4400, src("app", "app.serve(port=)"))
    monkeypatch.setenv("CEREYAN_PORT", "4500")
    assert serve_mod._host_port(filed, None, None, app_port=4400)[1::2] == (4500, src("env", "CEREYAN_PORT"))
    assert serve_mod._host_port(filed, None, 4600, app_port=4400)[1::2] == (4600, src("flag", "--port"))


def test_token_source_without_the_value(tmp_path, monkeypatch):
    monkeypatch.setenv("CEREYAN_TOKEN", "s3cret")
    assert serve_mod._token(project(tmp_path, "p")) == ("s3cret", src("env", "CEREYAN_TOKEN"))
    monkeypatch.delenv("CEREYAN_TOKEN")
    assert serve_mod._token(project(tmp_path, "q")) == (None, src("default"))


def test_string_settings_name_their_source(tmp_path):
    directory = project(tmp_path, "p", '[server]\nauth_scope = "api"\nlogin_url = "/login"\n')
    assert serve_mod._auth_scope(directory) == ("api", src("toml", "[server] auth_scope"))
    assert serve_mod._login_url(directory, "/signin") == ("/signin", src("flag", "--login-url"))
    assert serve_mod._auth_cookie(directory) == (None, src("default"))


def test_error_messages_still_name_the_file(tmp_path):
    directory = project(tmp_path, "p", "[server]\nbase_path = 5\n")
    with pytest.raises(serve_mod.CereyanError, match=r"from \[server\] base_path in cereyan.toml"):
        serve_mod.resolve_base_path(directory)


def test_enable_auth_sources(tmp_path, monkeypatch):
    bare = project(tmp_path, "bare")
    assert serve_mod._enable_auth(bare) == (False, src("default"))
    assert serve_mod._enable_auth(bare, app_enable_auth=True) == (True, src("app", "app.serve(enable_auth=)"))
    monkeypatch.setenv("CEREYAN_ENABLE_AUTH", "no")
    assert serve_mod._enable_auth(bare, app_enable_auth=True) == (False, src("env", "CEREYAN_ENABLE_AUTH"))
    assert serve_mod._enable_auth(bare, True) == (True, src("flag", "--enable-auth"))


def test_file_only_settings(tmp_path):
    directory = project(
        tmp_path, "p",
        '[defaults]\nretain_days = 7\n[ui]\ntitle = "Ops"\n[resources]\ngpu = 1\n'
        '[email]\nhost = "smtp"\nfrom = "a@b"\n',
    )
    sources = serve_mod._file_sources(directory, {}, {"retain_days": 7})
    assert sources["defaults.retain_days"] == src("toml", "[defaults] retain_days")
    assert sources["defaults.catchup"] == src("default")
    assert sources["server.cancel_grace_secs"] == src("default")
    assert sources["ui.title"] == src("toml", "[ui] title")
    assert sources["resources.gpu"] == src("toml", "[resources] gpu")
    assert sources["email.host"] == src("toml", "[email] host")
