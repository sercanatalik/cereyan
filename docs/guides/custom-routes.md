# How to add custom HTTP routes

Register handlers on an App and the server serves them next to its own API: a webhook that starts a run, a health check for your load balancer, a small JSON endpoint over your data. Handlers are FastAPI-shaped functions; routing and the HTTP server are in Rust.

## Register a route

```python
from dataclasses import dataclass
from cereyan import App, HTTPError, Request, Response
import cereyan

app = App("intake")

@dataclass
class Order:
    order_id: int
    amount: float

@app.get("/api/ext/ping")
def ping() -> dict:
    return {"ok": True}

@app.get("/api/ext/orders/{order_id}")
def order(order_id: int, verbose: bool = False) -> dict:
    if order_id < 1:
        raise HTTPError(404, "no such order")
    return {"order_id": order_id, "verbose": verbose}

@app.post("/api/ext/orders")
def receive(order: Order) -> tuple[dict, int]:
    run = cereyan.client.run("ingest_order", order_id=order.order_id, amount=order.amount)
    return {"run_id": run["id"]}, 201

@app.get("/api/ext/raw")
def raw(request: Request) -> Response:
    return Response(request.headers.get("user-agent", ""), media_type="text/plain")

assert [r.path for r in app.routes] == ["/api/ext/ping", "/api/ext/orders/{order_id}", "/api/ext/orders", "/api/ext/raw"]
```

`@app.get`, `@app.post`, `@app.put`, `@app.patch`, `@app.delete`, and `@app.route(method, path)` register a handler. The routes are served when the module is served with `cereyan serve` or `app.serve()`.

## How arguments bind

| Parameter | Bound from |
|---|---|
| Name in the path template (`{order_id}`) | The path segment, coerced through the type hint |
| Other scalar parameter | The query string; `list[T]` collects repeated keys; missing ones use the default or answer 422 |
| Dataclass, `TypedDict`, or pydantic model | The JSON body |
| Annotated `Request` | The raw request: `method`, `path`, `path_params`, `query`, `headers`, `body`, `json()`, `text()` |

Coercion follows the same rules as flow parameters; a value that does not coerce answers 422 with the reason.

## What to return

| Return | Response |
|---|---|
| `dict` or `list` | JSON, 200 |
| `(value, status)` | The value with that status |
| `str` | `text/plain` |
| `bytes` | `application/octet-stream` |
| `Response(body, status, headers, media_type)` | As given |
| `None` | JSON `null` |

Raise `HTTPError(status, message)` for an error; an unhandled exception answers 500 with the traceback in the server log.

## Call it

```{.python fixture:served}
import json, urllib.request

with urllib.request.urlopen(served.url + "/api/ext/ping") as resp:
    assert json.loads(resp.read()) == {"ok": True}

body = json.dumps({"order_id": 7, "amount": 99.5}).encode()
req = urllib.request.Request(served.url + "/api/ext/orders", data=body, method="POST", headers={"content-type": "application/json"})
with urllib.request.urlopen(req) as resp:
    assert resp.status == 201
    run_id = json.loads(resp.read())["run_id"]
assert served.wait_run(run_id)["state"]["type"] == "Completed"
```

## Async handlers

`async def` handlers are supported and run on one shared event loop, so a blocking call inside one blocks every other async handler. Keep them non-blocking, or use a sync handler, which runs on its own thread. An async handler that takes longer than 30 seconds answers 504.

```python
from cereyan import App

app = App("async-intake")

@app.get("/api/ext/status")
async def status() -> dict:
    return {"ok": True}
```

## Where routes appear

The Settings page lists every registered route with its source. Routes under `/api/` are protected by the API token like the built-in API; routes elsewhere are open. Starting runs from a route goes through `cereyan.client`, so the run records `created_by = client`.

Related: [Secure the server](secure-the-server.md), the [webhook route example](../examples/webhook_route.md).
