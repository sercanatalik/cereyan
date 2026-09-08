# Artifacts

```python
from cereyan import flow, task, artifacts

@task
def load(rows: list[dict]) -> int:
    artifacts.create_table(rows, key="loaded-rows")
    artifacts.create_progress(100, key="load", label="load")
    return len(rows)

@flow
def etl() -> int:
    n = load([{"day": "2026-09-06", "rows": 3}])
    artifacts.create_markdown(f"Loaded **{n}** batch(es).")
    return n

assert etl() == 1
```

An **artifact** is a small record a run or task run publishes for people to look at: a markdown note, a table, a progress bar, a link, or an image. Artifacts show on the run page's Artifacts tab and, across runs, on the Artifacts page.

## Kinds

| Function | Shows |
|---|---|
| `create_markdown(text)` | Rendered markdown |
| `create_table(rows, columns=None)` | A table from a list of dicts or a list of lists |
| `create_progress(percent, label=None)` and `update_progress(key, percent)` | A progress bar; updates under the same key keep a history |
| `create_link(url, text=None)` | A link |
| `create_image(url_or_bytes, media_type="image/png")` | An image by URL or embedded bytes |

Each call returns the artifact id. An artifact is limited to 1 MB and can only be created inside a run.

## Keys

A `key` groups artifacts across runs: every artifact published under `loaded-rows` forms a history you can open from the Artifacts page, newest first. Use keys for values you want to track over time, such as row counts or data-quality scores; leave them off for one-off notes.

## Reading artifacts

`GET /api/runs/{id}/artifacts` and `GET /api/task-runs/{id}/artifacts` return a run's artifacts; `GET /api/artifacts` lists them across runs with filters for kind, key, flow, and project and keyset pagination. The MCP `list_artifacts` tool and the `cereyan://runs/{id}/artifacts` resource expose the same to agents. Retention does not delete artifacts; deleting a run deletes its artifacts.

Related: [Publish artifacts](../guides/artifacts.md), [Variables](variables.md).
