# ---
# title: Agent diagnosis
# description: Start a failing run through the MCP endpoint and ask the server to explain it, the way an agent would.
# order: 7
# fixture: served
# ---
#
# The server's MCP endpoint gives an agent a curated set of tools. This script does
# what an agent host does over Streamable HTTP: initialise a session, start a run
# with `run_flow`, wait for it, and call `explain_failure` to get the run, its
# failed task runs, the last error logs, and its events in one answer. Run it
# against `cereyan serve examples/`.

import json
import time
import urllib.request

from cereyan import client, flow, get_run_logger, task


@task
def load(rows: int) -> int:
    if rows > 3:
        raise ValueError(f"too many rows: {rows} (limit 3)")
    return rows


# ## A flow that fails for some inputs


@flow
def flaky_load(rows: int = 1) -> int:
    get_run_logger().info("loading %d rows", rows)
    return load(rows)


# ## A minimal MCP client
#
# One JSON-RPC message per POST. The `initialize` reply carries an `Mcp-Session-Id`
# header that later requests send back; the client name from the handshake is what
# runs record as `created_by`.


class Mcp:
    def __init__(self, url: str) -> None:
        self.url = url + "/mcp"
        self.session = None
        self.counter = 0

    def rpc(self, method: str, params: dict | None = None, *, notification: bool = False):
        self.counter += 1
        msg = {"jsonrpc": "2.0", "method": method, "params": params or {}}
        if not notification:
            msg["id"] = self.counter
        headers = {"content-type": "application/json"}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        req = urllib.request.Request(self.url, data=json.dumps(msg).encode(), headers=headers, method="POST")
        with urllib.request.urlopen(req, timeout=10) as resp:
            self.session = resp.headers.get("Mcp-Session-Id") or self.session
            raw = resp.read()
        return json.loads(raw) if raw else None

    def call(self, tool: str, **arguments):
        result = self.rpc("tools/call", {"name": tool, "arguments": arguments})["result"]
        text = result["content"][0]["text"]
        return json.loads(text) if not result.get("isError") else {"error": text}


# ## Diagnose a failure

if __name__ == "__main__":
    mcp = Mcp(client.read_discovery()["url"])
    mcp.rpc("initialize", {"protocolVersion": "2025-06-18", "clientInfo": {"name": "example-agent", "version": "1"}, "capabilities": {}})
    mcp.rpc("notifications/initialized", notification=True)

    run = mcp.call("run_flow", flow="flaky_load", parameters={"rows": 5})["run"]
    assert run["created_by"] == "mcp:example-agent"
    deadline = time.time() + 30
    while time.time() < deadline and client.get_run(run["id"])["state"]["type"] not in ("Completed", "Failed"):
        time.sleep(0.1)

    explained = mcp.call("explain_failure", run_id=run["id"])
    print("verdict:", explained["verdict"])
    assert "too many rows" in explained["verdict"]
    assert any(e["name"] == "run.failed" for e in explained["events"])
