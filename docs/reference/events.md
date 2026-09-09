# Events

Every meaningful change is recorded as an event with a name, a sequence number, an occurred time, a resource (`run`, `task_run`, `flow`, `schedule`, `rule`, or `custom`), related resources, and a JSON payload. Rules match on events, the Events page and `GET /api/events` list them, and new ones arrive on the SSE stream as `event.created`. Names are assigned in `crates/server/src/events.rs` (run and task-run transitions) and at the emitting sites in `crates/server/src/{dispatch,scheduler,rules,supervisor}.rs`; offline, `python/cereyan/engine/backends.py` records the run and task-run events with a smaller payload. This page mirrors those files.

## Run events

Resource `run`; related `flow` and the run's tags. Payload on the served path: `state` (the state name), `state_type`, `message`, `flow`, `project`, `parameters`, `created_by`. Offline the payload is `state` and `message`.

| Event | When |
|---|---|
| `run.scheduled` | The run is created, or re-enters Scheduled for a crash rerun |
| `run.pending` | An engine accepted the run |
| `run.running` | User code started |
| `run.completed` | The run finished without error |
| `run.failed` | The run raised, timed out, or was set failed |
| `run.crashed` | The engine died while the run was executing |
| `run.cancelled` | The run was cancelled |
| `run.late` | The scheduled time passed 15 seconds ago and the run has not started; payload also carries `scheduled_time` and `name` |
| `run.retrying` | A retry attempt started |
| `run.skipped` | The run ended Skipped: `on_overlap="skip"` (payload `reason`), a backfill value already done, or a catch-up drop |
| `run.paused` | The run is waiting on `wait_for_input` |
| `run.resumed` | The run was answered and its next attempt scheduled |

`AwaitingRetry`, `AwaitingResource`, and `Cancelling` record no event.

## Task run events

Resource `task_run` (id is the task run's external id, name its dynamic key); related `run` and `flow`. Payload: `task`, `dynamic_key`, `state`, `message`, `flow`, `project`. Offline the payload is `state` and `task_run`.

| Event | When |
|---|---|
| `task_run.running` | The task started |
| `task_run.completed` | The task returned |
| `task_run.failed` | The task raised or timed out (after its retries) |
| `task_run.cancelled` | The task was cancelled with its run |
| `task_run.skipped` | The task's `output=` target already existed |
| `task_run.cached` | The task returned a persisted result |

## Flow events

| Event | Resource | Payload | When |
|---|---|---|---|
| `flow.registered` | `flow` | `flow`, `project`, `module` | The server registered the flow at start or on handoff |
| `flow.disabled` | `flow` | `failures`, `window_seconds`, `until` | `disable_after` tripped; the flow's schedules are paused until `until` |
| `flow.enabled` | `flow` | empty | The disable window ended and the schedules resumed |
| `flow.fan_in` | `flow` (the downstream) | `key`, `value`, the upstream runs, and `run_id` of the run created | Every upstream completed a run for the key value and the downstream run was created |

## Schedule events

Resource `schedule`, related `flow`.

| Event | Payload | When |
|---|---|---|
| `schedule.paused` | `schedule_id`, `reason` (`user`, `disabled`) | A schedule was paused from the UI, the API, an MCP tool, a rule, or a disable window |
| `schedule.resumed` | `schedule_id` | A schedule was resumed |
| `schedule.catchup` | `schedule_id`, `policy`, `missed`, `created`, `dropped` | The server started and applied the catch-up policy to fires missed while it was down |

## Resource events

| Event | Payload | When |
|---|---|---|
| `resource.exhausted` | `resource` | A run waited for a resource that had no capacity; recorded once per wait |

## Rule events

Resource `rule`, related the run and flow of the triggering event.

| Event | Payload | When |
|---|---|---|
| `rule.fired` | `rule_id`, `rule`, `event`, `event_id` | A rule matched an event and its actions started |
| `rule.action.completed` | `rule_id`, `action`, `index`, `detail` | One action finished |
| `rule.action.failed` | `rule_id`, `action`, `index`, `error` | One action failed, including a template that did not render |
| `expectation.armed` | `id`, `rule_id`, `key`, `run_id`, `deadline` | A proactive rule's `when` event armed an expectation |
| `expectation.met` | `id`, `rule_id`, `key` | The expected event arrived before the deadline |
| `expectation.lapsed` | `rule_id`, `rule`, `flow`, `project`, `run`, `run_name`, `expected`, `deadline`, `armed_at`, `expectation_id` | The deadline passed, or a clock-armed rule's tick found no matching event; the rule's actions run against this event |

Runs created by a rule record `created_by = rule:<id>`, and a rule never fires on events of runs it created unless `allow_self` is set.

## Custom events

`cereyan.emit_event(name, payload=None, resource=None)` records any name. Inside a run the resource defaults to that run and the task run is recorded; outside a run the event goes to the store, or to the server's `POST /api/events` when one holds the store. Names with a dot-separated prefix, such as `orders.table_empty`, match rules with `events: ["orders.*"]`.

## Stream messages

The SSE stream at `GET /api/stream` carries live notifications for the UI. They are not stored events and rules cannot match them, even where a name is shared with one.

| Message | Data | Sent when |
|---|---|---|
| `hello` | `latest`, `since` | The connection opens, before any backlog |
| `run.updated` | The run | A run was created or changed state |
| `task_run.updated` | The task run | A task run was created or changed state |
| `log.appended` | `run_id`, `last_id`, `count` | New log lines arrived |
| `event.created` | The event | Any event was recorded |
| `flow.registered` | The flow | A flow was registered or re-registered |
| `rule.updated` | The rule, or `id` and `deleted` | A rule was created, edited, fired, or deleted |
| `variable.updated` | The variable, or `name` and `deleted` | A variable was set or removed |
| `artifact.updated` | The artifact | A run published or updated an artifact |
| `schedule.updated` | The schedule, or `id` and `deleted` | A schedule was created, edited, paused, resumed, or deleted |
| `backfill.created` | `backfill_id`, `flow_id`, `count` | A backfill created its runs |
| `backfill.updated` | `backfill_id`, `cancelled` | A backfill was cancelled |
| `expectation.armed`, `expectation.met`, `expectation.lapsed` | `id`, `rule_id`, and the key or the event | A proactive rule armed, disarmed, or lapsed an expectation |
| `resync` | `latest` | The client asked for a sequence number older than the replay buffer; refetch everything |

Each message carries the sequence number as its SSE id. Reconnect with `?since=<seq>` to replay what was missed, or receive `resync` when the buffer no longer reaches back that far.
