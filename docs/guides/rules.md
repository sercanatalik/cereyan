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

A `states` value matches the run's state type or its sub-state name, so `["Completed"]` above also covers a run that ended `Skipped`, and `["Skipped"]` would narrow to just those.

Event and state names are checked against the [catalogue](../reference/events.md) before the rule is stored. A name under one of the engine's prefixes (`run.`, `task_run.`, `flow.`, `schedule.`, `resource.`, `rule.`, `expectation.`) that nothing emits is refused with the nearest match, so a rule cannot be saved dead:

```{.python fixture:served}
from cereyan.client import ApiError

try:
    served.client.create_rule({
        "name": "typo", "when": {"events": ["run.failure"]},
        "do": [{"kind": "cancel_run"}],
    })
except ApiError as exc:
    assert "run.failed" in str(exc)
```

Any name outside those prefixes is a custom event of yours and is accepted as typed.

## Actions

| Kind | Fields | Effect |
|---|---|---|
| `run_flow` | `flow`, `parameters` (templated), `delay` (seconds) | Create a run, now or `delay` seconds later; it records `created_by = rule:<id>` |
| `cancel_run` | | Cancel the run the event is about |
| `cancel_runs` | `flow` (templated, defaults to the event's flow), `parameters` (templated), `states` | Cancel every active run of the flow whose parameters equal the rendered values, in the given state types (all non-terminal ones by default), never the event's own run; the outcome lists the ids |
| `set_state` | `state_type`, `message` | Force the run into a state |
| `pause_schedule`, `resume_schedule` | `schedule_id` | Stop or restart a schedule |
| `webhook` | `url`, `method`, `headers`, `body` (templated) | HTTP request; three attempts with backoff |
| `email` | `to`, `subject`, `body` (templated) | Sent through `[email]` in `cereyan.toml` |
| `call` | `callable` | A code rule's function |

Actions run in order; a failing action is recorded as `rule.action.failed` and the rest still run. A rule on a custom event can stop work that has become pointless: on `orders.cancelled`, `cancel_runs` with `flow: fulfil` and `parameters: {"order": "{{ payload.order }}"}` cancels the `fulfil` runs of that order and leaves the others alone. A string in the selector also matches a parameter of another type with the same text, so `{{ payload.id }}` matches an integer parameter.

While the scheduler is [paused for maintenance](../concepts/schedules.md#pausing-everything) with `suppress_rules`, a rule that would fire is recorded as a firing whose actions are `suppressed`, nothing runs, and nobody is paged about the failures the maintenance itself causes.

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

### Presets, signing, and links

A webhook does not need a body for the common receivers: `"preset": "slack"`, `"teams"`, `"discord"`, `"ntfy"` (with `"topic"`), `"telegram"` (with `"chat_id"`; the URL is the bot's `sendMessage` endpoint), or `"pagerduty"` (with `"routing_key"`; the URL defaults to the Events API) sends that receiver's message: the flow, the event, the failure message, and a link to the run. A body of your own always wins over the preset. The PagerDuty preset triggers on a failure and resolves on `flow.recovered`, both under a `dedup_key` of `cereyan-<project>-<flow>`, so one rule on `["run.failed", "flow.recovered"]` opens and closes the incident.

```json
{"kind": "webhook", "url": "https://hooks.slack.com/services/...", "preset": "slack", "secret": "change-me"}
```

With `secret` set, every call carries `webhook-id`, `webhook-timestamp`, and `webhook-signature` (`v1,` and a base64 HMAC-SHA256 over `id.timestamp.body`), the [Standard Webhooks](https://github.com/standard-webhooks/standard-webhooks) form, so the receiver can verify it came from your server. Templates can link to a run with `{{ run.url }}`, which is `[server] public_url` (or `--public-url`, `CEREYAN_PUBLIC_URL`, `app.serve(public_url=)`) followed by `/runs/<id>`, or the server's own address when no public URL is set.

## Write a code rule

```python
from cereyan import App, events

app = App("ops")

@app.rule(on=events.run.failed, flow="nightly", once="per_run", cooldown_seconds=60)
def on_nightly_failure(event, run):
    print(f"{run['name']} failed: {event['payload'].get('message')}")

assert app.rules[0].spec()["when"]["flows"] == ["nightly"]
assert app.rules[0].spec()["when"]["events"] == ["run.failed"]
```

`events.run.failed` is the string `"run.failed"`, so the constants and the literals are interchangeable; the constants just autocomplete and catch a typo at the point you write it. `events.run.any` is `"run.*"`, and `cereyan.states` does the same for `states=`.

The function receives the event and the run as dicts and its return value is recorded with the firing. Code rules are re-registered on every server start, show read-only on the Rules page with a `code` badge, and fire on the offline path for a script's own events once a server has registered them. Guards are keyword arguments: `once`, `cooldown_seconds`, `max_per_minute`, `allow_self`, and `name` — anything else raises, so a misspelled guard cannot silently leave the rule on its defaults.

## Guards

- `once="per_run"` (the default) fires at most once per run, so a run that retries three times alerts once. `once="never"` lifts the limit and fires on every matching event.
- `cooldown_seconds` and `max_per_minute` throttle noisy rules.
- `after_consecutive=3` fires only on an event whose `failures_in_a_row` is at least three: `run.failed` and `run.crashed` carry that count for served runs, and `flow.recovered` reports how many failures a success ended, so "page on the third failure in a row, and again when it recovers" is two rules.
- A rule never fires on events of runs it created, unless `allow_self` is set; this is what stops a `run_flow` rule looping.
- Disabled rules never fire. Toggle them on the Rules page or with `PATCH /api/rules/{id}`.

A rule whose names are all valid can still match nothing — the wrong flow, a tag that is never set. The Rules page marks any rule that has never fired, which is the one thing name checking cannot tell you.

## See what fired

The rule page lists firings with each action's outcome; `GET /api/rules/{id}/firings` returns them. Every firing also records `rule.fired` and per-action `rule.action.completed` or `rule.action.failed` events.

Related: [Events and rules](../concepts/events-and-rules.md), [Detect when something did not happen](detect-missing-events.md).
