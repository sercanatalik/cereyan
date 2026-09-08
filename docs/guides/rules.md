# How to react to events with rules

A rule matches events and runs actions: start a flow, cancel a run, force a state, pause a schedule, call a webhook, send an email, or call your function. Data rules are created in the UI or through the API and edited at runtime; code rules live next to the flows.

## Create a data rule

On the Rules page, **New rule** opens the form. The same body goes to `POST /api/rules`:

```{.python fixture:served}
rule = served.client.create_rule({
    "name": "cleanup after etl",
    "when": {"events": ["run.*"], "flows": ["etl"], "states": ["Completed"]},
    "do": [{"kind": "run_flow", "flow": "etl", "parameters": {"day": "{{ run.parameters.day }}"}}],
    "once": "per_run",
    "cooldown_seconds": 0,
    "max_per_minute": 60,
})
assert rule["name"] == "cleanup after etl"
assert any(r["id"] == rule["id"] for r in served.client.rules())
```

`when` takes `events` (names or prefixes such as `run.*`), `flows`, `tags`, `states`, and `project`; a rule fires for an event that matches every clause it sets.

## Actions

| Kind | Fields | Effect |
|---|---|---|
| `run_flow` | `flow`, `parameters` (templated) | Create a run; it records `created_by = rule:<id>` |
| `cancel_run` | | Cancel the run the event is about |
| `set_state` | `state_type`, `message` | Force the run into a state |
| `pause_schedule`, `resume_schedule` | `schedule_id` | Stop or restart a schedule |
| `webhook` | `url`, `method`, `headers`, `body` (templated) | HTTP request; three attempts with backoff |
| `email` | `to`, `subject`, `body` (templated) | Sent through `[email]` in `cereyan.toml` |
| `call` | `callable` | A code rule's function |

Actions run in order; a failing action is recorded as `rule.action.failed` and the rest still run.

## Templates

Fields marked templated use Jinja syntax, rendered in Rust, with `event`, `run`, `flow`, `state`, `payload`, and `parameters` in scope:

```text
{{ flow.name }} run {{ run.name }} ended {{ state.type }}: {{ state.message }}
{{ run.parameters.day }}
{{ payload.table }}
```

An undefined variable is an error for that action only. **Test** on the rule page (`POST /api/rules/{id}/test`) renders the templates against the most recent matching event without executing anything.

## Notify by webhook or email

```json
{
  "name": "page on nightly failure",
  "when": {"events": ["run.failed"], "flows": ["nightly"]},
  "do": [
    {"kind": "webhook", "url": "https://hooks.example.com/pager", "method": "POST",
     "body": "{\"text\": \"{{ flow.name }} failed: {{ state.message }}\"}"},
    {"kind": "email", "to": "oncall@example.com", "subject": "{{ flow.name }} failed",
     "body": "Run {{ run.name }} failed with {{ state.message }}."}
  ],
  "once": "per_run"
}
```

Email needs `[email]` configured; see [Configuration](../reference/configuration.md).

## Write a code rule

```python
from cereyan import App

app = App("ops")

@app.rule(on="run.failed", flow="nightly", once="per_run", cooldown_seconds=60)
def on_nightly_failure(event, run):
    print(f"{run['name']} failed: {event['payload'].get('message')}")

assert app.rules[0].spec()["when"]["flows"] == ["nightly"]
```

The function receives the event and the run as dicts and its return value is recorded with the firing. Code rules are re-registered on every server start, show read-only on the Rules page with a `code` badge, and fire on the offline path for a script's own events once a server has registered them. Guards are keyword arguments: `once`, `cooldown_seconds`, `max_per_minute`, `allow_self`, and `name`.

## Guards

- `once="per_run"` fires at most once per run, so a run that retries three times alerts once.
- `cooldown_seconds` and `max_per_minute` throttle noisy rules.
- A rule never fires on events of runs it created, unless `allow_self` is set; this is what stops a `run_flow` rule looping.
- Disabled rules never fire. Toggle them on the Rules page or with `PATCH /api/rules/{id}`.

## See what fired

The rule page lists firings with each action's outcome; `GET /api/rules/{id}/firings` returns them. Every firing also records `rule.fired` and per-action `rule.action.completed` or `rule.action.failed` events.

Related: [Events and rules](../concepts/events-and-rules.md), [Detect when something did not happen](detect-missing-events.md).
