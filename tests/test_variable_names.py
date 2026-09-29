"""Variable names with '/' work through a server, as they do offline.

Served reads used to put the name into the URL unencoded, so `warehouse/api_token`
addressed a route that does not exist and `Variable.get` returned the default.
"""

from __future__ import annotations

import pytest

from cereyan import Variable
from server_helpers import ServerProcess

PIPELINE = '''
from cereyan import App, Variable

app = App("names")

@app.flow
def reads():
    token = Variable.get("warehouse/api_token")
    region = Variable.get("config/eu.west")
    assert token == "s3cr3t-value", f"secret was {token!r}"
    assert region == {"zone": 2}, f"plain was {region!r}"
'''


@pytest.fixture
def srv(isolated_home, tmp_path):
    from cereyan import engine

    d = tmp_path / "names"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    engine.close_store()
    server = ServerProcess(str(isolated_home), str(d))
    try:
        yield server
    finally:
        server.stop()


def test_a_served_run_reads_variables_whose_names_have_a_slash(srv):
    Variable.set("warehouse/api_token", "s3cr3t-value", secret=True)
    Variable.set("config/eu.west", {"zone": 2})
    run = srv.client.run("reads")
    done = srv.wait_run(run["id"])
    assert done["state"]["type"] == "Completed", done["state"].get("message")


def test_reading_and_deleting_through_the_server(srv):
    Variable.set("team/a/b", [1, 2])
    assert Variable.get("team/a/b") == [1, 2]
    assert Variable.unset("team/a/b") is True
    assert Variable.get("team/a/b", "gone") == "gone"
    assert Variable.unset("team/a/b") is False
