# How to publish artifacts

Publish a table, a note, a progress bar, a link, or an image from a run so the result is visible on the run page and, under a key, tracked across runs.

## From a task or a flow

```python
from cereyan import flow, task, artifacts

@task
def validate(rows: list[dict]) -> int:
    bad = [r for r in rows if r["amount"] < 0]
    artifacts.create_table(bad, key="bad-rows", columns=["id", "amount"])
    artifacts.create_markdown(f"{len(bad)} of {len(rows)} rows rejected")
    return len(rows) - len(bad)

@flow
def load(rows: list[dict] | None = None) -> int:
    rows = rows or [{"id": 1, "amount": 10}, {"id": 2, "amount": -3}]
    artifacts.create_link("https://example.com/dashboards/loads", text="Load dashboard")
    return validate(rows)

assert load() == 1
```

Calls inside a task attach the artifact to the task run; calls in the flow body attach it to the run. Each returns the artifact id and raises `CereyanError` outside a run or over 1 MB.

## Report progress

```python
from cereyan import flow, task, artifacts

@task
def process(batches: int) -> None:
    artifacts.create_progress(0, key="process", label="batches")
    for i in range(batches):
        artifacts.update_progress("process", 100 * (i + 1) / batches)

@flow
def big_load() -> None:
    process(4)

big_load()
```

Each update is a new artifact under the key, so the run page shows the latest value and the Artifacts page keeps the history.

## Embed an image

```python
from cereyan import flow, artifacts

@flow
def chart() -> None:
    png = b"\x89PNG\r\n\x1a\n" + b"\x00" * 16   # bytes from your plotting library
    artifacts.create_image(png, key="daily-chart", media_type="image/png")
    artifacts.create_image("https://example.com/chart.png")

chart()
```

Bytes are embedded as a data URI, so keep images small; link large ones by URL.

## Track a value over time

Give artifacts that recur a stable `key`. The Artifacts page filters by kind, key, flow, and project, and opening a key shows every value published under it across runs, newest first. Row counts, data-quality scores, and file sizes are good keys.

## Read them programmatically

`GET /api/runs/{id}/artifacts`, `Client.artifacts(run_id)`, `GET /api/artifacts?key=...`, the MCP `list_artifacts` tool, and the `cereyan://runs/{id}/artifacts` resource return artifacts as JSON:

```{.python fixture:served}
run = served.client.run("etl", day="2026-02-01")
served.wait_run(run["id"])
items = served.client.artifacts(run["id"])
assert isinstance(items, list)
```

Related: [Artifacts](../concepts/artifacts.md).
