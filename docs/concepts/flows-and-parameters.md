# Flows and parameters

```python
from datetime import date
from cereyan import flow

@flow(run_name="etl-{day}", tags=["example"])
def etl(day: date, full: bool = False) -> str:
    return f"{day} full={full}"

assert etl(day="2026-01-02", full="yes") == "2026-01-02 full=True"
```

A **flow** is a decorated function. Calling it starts a **run**: the tasks it calls are recorded as task runs, its logs are stored, and it ends in a terminal state. Offline the run is recorded in the local store. While a server holds the store, the call hands the run to the server and streams the logs back. A flow's identity is `(project, name)`; see [App and projects](app-and-projects.md).

## Parameters come from type hints

Values arrive from the decorator call, the CLI (`--param day=2026-01-02`), the API and the UI run form (JSON), a schedule's defaults, or a backfill. Whatever the source, they are coerced to the declared types before the body runs, and a value that does not coerce raises `ParameterError` (exit code 3 on the CLI, 422 on the API) before anything is recorded as started.

| Hint | Accepted values |
|---|---|
| `str`, `int`, `float` | The type, or a string that parses |
| `bool` | `true`, `1`, `yes`, `on`, `y`, `t` and their negatives, case-insensitive |
| `date`, `datetime` | ISO 8601 strings |
| `timedelta` | Seconds, `MM:SS`, or `HH:MM:SS` |
| `Optional[T]`, `T | None` | `null` or `None`, else coerced as `T` |
| `list[T]`, `dict[str, T]` | JSON on the CLI; elements coerced as `T` |
| `Literal[...]` | One of the listed values |
| `Enum` | The member's value, or its name |
| dataclasses | A JSON object; fields coerced by their hints |

Any other hint is opaque: the value passes through unchanged and the schema describes it as untyped. The schema derived from the hints drives the UI's run form and is exposed by the API as `parameter_schema`.

## Naming runs

Runs get a random `adjective-animal` name unless the flow sets `run_name`: a `str.format` template over the parameters, or a callable receiving them as keyword arguments. Names need not be unique.

## What a flow can declare

| Concern | Options | Where |
|---|---|---|
| When it runs | `schedule`, `schedules` | [Schedules](schedules.md) |
| Reliability | `retries`, `retry_delay`, `timeout_seconds`, `crash_retries` | [Retry, time out and survive crashes](../guides/retries-timeouts-crashes.md) |
| Hooks | `on_completion`, `on_failure`, `on_crashed`, `on_cancellation` | [Run code on state changes](../guides/state-hooks.md) |
| Concurrency | `max_concurrent`, `on_overlap`, `resources`, `priority`, `disable_after` | [Resources and concurrency](resources-and-concurrency.md) |
| Dependencies | `after`, `batch_key` | [Dependencies](dependencies.md) |
| Backfills | `bulk_complete` | [Backfills](backfills.md) |
| Execution | `runner`, `isolated`, `log_prints` | [Tasks](tasks.md), [Engines and the home directory](engines-and-home.md) |
| Display | `name`, `description`, `tags` | |

The full option list with types and defaults is in the [Python API reference](../reference/python-api.md#cereyan.flow).

## The flow object

`@flow` returns a `Flow`, not the function. It is still callable, and it exposes what was derived: `parameters`, `schema`, `project`, `name`, `tags`, and `options`. Use it in tests to call the flow directly or to read its schema.
