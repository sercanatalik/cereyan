# How to fetch from an HTTP API

Build the client once, submit one task run per request, and let the runner fetch them concurrently.

## Build the client once

An engine imports your module once and serves many runs from that import, so a client built at module level keeps its connection pool and TLS sessions across every run that engine serves. Building one inside the task body pays a fresh handshake on every call.

The blocks below fetch from a small local server so the page can be tested; in your own pipeline `BASE` is the API you call.

```python
import http.server, json, threading, urllib.request

from cereyan import flow, task

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"path": self.path}).encode()
        self.send_response(200)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

api = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=api.serve_forever, daemon=True).start()
BASE = f"http://127.0.0.1:{api.server_port}"

opener = urllib.request.build_opener()  # built once per import, not once per run

@task(retries=2, timeout_seconds=10)
def fetch(path: str) -> dict:
    with opener.open(BASE + path, timeout=10) as resp:
        return json.loads(resp.read())

@flow
def one() -> dict:
    return fetch("/orders")

assert one() == {"path": "/orders"}
```

`httpx` and `requests` work the same way: build the `Client` or `Session` at module level and call it from the task body. Both are safe to share across the threads a `ThreadRunner` uses.

## Fetch concurrently

`map` submits one task run per element. Each one is recorded, retried and timed on its own, and the timeline shows them side by side.

```{.python continuation}
from cereyan import ThreadRunner

@flow(runner=ThreadRunner(max_workers=4))
def collect() -> list[str]:
    futures = fetch.map(["/orders", "/customers", "/products"])
    return [f.result()["path"] for f in futures]

assert collect() == ["/orders", "/customers", "/products"]
```

`ThreadRunner` is the default and suits HTTP, because the calls release the GIL while they wait. Raise `max_workers` for more requests in flight; it defaults to the CPU count. [Run tasks concurrently](concurrent-tasks.md) covers futures, `wait_for` and runners in full.

## Retry and time out

`retries` and `timeout_seconds` on `@task` apply per request, so one slow endpoint does not fail the whole run. A task run that passes `timeout_seconds` ends Failed with sub-state `TimedOut`; under a `ThreadRunner` the request itself may still be in flight, so set a timeout on the client as well, as the blocks above do.

## Async clients are not supported

`@flow` and `@task` reject `async def`, with or without `yield`. Bodies run synchronously, so the coroutine would never be awaited and the run would be recorded as having succeeded without fetching anything. Drive the loop yourself:

```python
import asyncio

from cereyan import task

async def _fetch(path: str) -> str:
    await asyncio.sleep(0)
    return path

@task
def fetch_one(path: str) -> str:
    return asyncio.run(_fetch(path))

assert fetch_one("/orders") == "/orders"
```

Each `asyncio.run` gets its own event loop, so an async client cannot be shared between task runs the way a synchronous one can. `async def` route handlers are unaffected; see [custom routes](custom-routes.md).

## When the client is not reused

| Situation | What happens |
|---|---|
| `@flow(isolated=True)` | A fresh process and a fresh import for every run |
| `ProcessRunner` | Every task run is a spawned process with its own import |
| `python pipeline.py` | One process, one run: reuse within the run only |

Related: [Run tasks concurrently](concurrent-tasks.md), [Engines and the home directory](../concepts/engines-and-home.md).
