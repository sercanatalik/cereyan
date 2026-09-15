"""The UI title: `[ui] title` in cereyan.toml, `/api/server`, the served page, and `PATCH /api/settings`."""

import urllib.request

import pytest
from cereyan.client import ApiError
from server_helpers import ServerProcess

PIPELINE = """
from cereyan import flow


@flow
def hello() -> int:
    return 1
"""


@pytest.fixture
def titled_dir(tmp_path):
    d = tmp_path / "titled"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    return d


def serve(home, directory):
    from cereyan import engine

    engine.close_store()
    return ServerProcess(str(home), str(directory))


def page_title(srv):
    with urllib.request.urlopen(srv.info["url"] + "/runs/42", timeout=5) as resp:
        html = resp.read().decode()
    return html.split("<title>", 1)[1].split("</title>", 1)[0]


def test_default_title(isolated_home, titled_dir):
    srv = serve(isolated_home, titled_dir)
    try:
        assert srv.client.server()["title"] == "cereyan"
        assert srv.client._request("GET", "/api/settings")["title"] == "cereyan"
        assert page_title(srv) == "cereyan"
    finally:
        srv.stop()


def test_configured_title_is_served_escaped_and_warns_about_nothing(isolated_home, titled_dir):
    (titled_dir / "cereyan.toml").write_text('[ui]\ntitle = "  Data & <Platform>  "\n')
    srv = serve(isolated_home, titled_dir)
    try:
        assert srv.client.server()["title"] == "Data & <Platform>"
        assert page_title(srv) == "Data &amp; &lt;Platform&gt;"
        assert "unknown" not in srv.read_log()
    finally:
        srv.stop()


def test_patch_writes_and_removes_the_title(isolated_home, titled_dir):
    toml = titled_dir / "cereyan.toml"
    toml.write_text("[defaults]\nretain_days = 9\n")
    srv = serve(isolated_home, titled_dir)
    try:
        saved = srv.client._request("PATCH", "/api/settings", body={"title": "Payments"})
        assert saved["title"] == "Payments" and saved["retain_days"] == 9
        assert srv.client.server()["title"] == "Payments"
        assert page_title(srv) == "Payments"
        text = toml.read_text()
        assert 'title = "Payments"' in text and "retain_days = 9" in text

        cleared = srv.client._request("PATCH", "/api/settings", body={"title": ""})
        assert cleared["title"] == "cereyan"
        assert "[ui]" not in toml.read_text() and "retain_days = 9" in toml.read_text()
    finally:
        srv.stop()


def test_invalid_title_is_rejected_and_leaves_the_title(isolated_home, titled_dir):
    srv = serve(isolated_home, titled_dir)
    try:
        srv.client._request("PATCH", "/api/settings", body={"title": "Ops"})
        for bad in ("x" * 81, "line\nbreak"):
            with pytest.raises(ApiError) as err:
                srv.client._request("PATCH", "/api/settings", body={"title": bad, "retain_days": 3})
            assert err.value.status == 422
        after = srv.client._request("GET", "/api/settings")
        assert after["title"] == "Ops" and after["retain_days"] != 3
    finally:
        srv.stop()


def test_invalid_title_at_startup_warns_and_falls_back(isolated_home, titled_dir):
    (titled_dir / "cereyan.toml").write_text(f'[ui]\ntitle = "{"x" * 81}"\n')
    srv = serve(isolated_home, titled_dir)
    try:
        assert srv.client.server()["title"] == "cereyan"
        assert "[ui] title ignored" in srv.read_log()
    finally:
        srv.stop()
