# Webhook route

A custom HTTP route that receives an order and starts a flow for it.

Source: [`examples/webhook_route.py`](https://github.com/sercanatalik/cereyan/blob/main/examples/webhook_route.py). Run it with `python examples/webhook_route.py` while `cereyan serve examples/` is running.

Custom routes are plain functions registered on an App and served next to the
API. Path and query parameters bind by name and are coerced through the type
hints; a dataclass parameter receives the JSON body. This route starts a run for
each order it receives. Run it against `cereyan serve examples/`.

```python
import json
import time
import urllib.request
from dataclasses import dataclass

from cereyan import App, client, get_run_logger

app = App("intake")


@dataclass
class Order:
    order_id: int
    amount: float
```

## The flow the route starts

```python
@app.flow
def ingest_order(order_id: int, amount: float) -> str:
    get_run_logger().info("order %d for %.2f", order_id, amount)
    return f"order {order_id} ingested"
```

## The routes

Return a dict for JSON, a `(value, status)` tuple to set the status, a str for
text, or a `Response` for full control. Raise `HTTPError` for an error status.

```python
@app.get("/api/ext/ping")
def ping() -> dict:
    return {"ok": True}


@app.post("/api/ext/orders")
def receive_order(order: Order) -> tuple[dict, int]:
    run = client.run("ingest_order", order_id=order.order_id, amount=order.amount)
    return {"run_id": run["id"]}, 201
```

## Call it

```python
if __name__ == "__main__":
    url = client.read_discovery()["url"]
    body = json.dumps({"order_id": 7, "amount": 99.5}).encode()
    req = urllib.request.Request(url + "/api/ext/orders", data=body, method="POST", headers={"content-type": "application/json"})
    with urllib.request.urlopen(req) as resp:
        assert resp.status == 201
        run_id = json.loads(resp.read())["run_id"]
    deadline = time.time() + 30
    while time.time() < deadline and client.get_run(run_id)["state"]["type"] not in ("Completed", "Failed"):
        time.sleep(0.1)
    print("run", run_id, client.get_run(run_id)["state"]["type"])
```
