# States and transitions

Runs and task runs move through the same states, and the same rules accept or reject every transition on both the offline path and the served path. The rules live in `crates/core/src/rules.rs` (`propose`) and the names in `crates/core/src/state.rs`; this page mirrors them.

## State types

| Type | Meaning | Terminal |
|---|---|---|
| `Scheduled` | Created and waiting for its time, a retry, or a resource | no |
| `Pending` | Dispatched to an engine, not yet executing | no |
| `Running` | Executing user code | no |
| `Paused` | Waiting for an answer to `wait_for_input`; the engine is released | no |
| `Cancelling` | Asked to stop; becomes Cancelled when the process ends or the grace period expires | no |
| `Completed` | Finished without error | yes |
| `Failed` | Raised, timed out, or was set failed | yes |
| `Cancelled` | Stopped on request | yes |
| `Crashed` | The engine died; rerun up to `crash_retries` times, then Failed | yes |

A state carries a `type`, a `name` (equal to the type unless it is a sub-state), an optional `message`, a `details` object, and a `timestamp` in microseconds.

A rule's `states` clause matches either: `states=["Scheduled"]` covers every scheduled run including `Late` and `AwaitingRetry`, and `states=["Late"]` narrows to that sub-state. `cereyan.states` exposes both sets as constants.

## Named sub-states

| Name | Type | When |
|---|---|---|
| `Late` | Scheduled | The scheduled time passed more than 15 seconds ago and the run has not started |
| `AwaitingRetry` | Scheduled | Waiting out `retry_delay` after a failure; increments `failure_count` |
| `AwaitingResource` | Scheduled | Waiting for a resource or the flow's concurrency cap |
| `Retrying` | Running | A retry attempt is executing |
| `TimedOut` | Failed | `timeout_seconds` elapsed |
| `Cached` | Completed | A task run returned a persisted result instead of executing |
| `Skipped` | Completed | A task run's target existed, a run hit `on_overlap="skip"`, a backfill value was already done, or catch-up dropped the run |

A resumed run re-enters `Scheduled` with the name `Resuming` before its next attempt.

## Transition rules

In order of evaluation:

1. A **forced** transition (`POST /api/runs/{id}/transition` with `force`, or a rule's `set_state` action) is always accepted and records `forced: true` in the state's details.
2. From a **terminal** state nothing is accepted.
3. The **same type and name within one second** of the current state is rejected as a duplicate.
4. Entry rules by proposed type:

| Proposed | Accepted from |
|---|---|
| `Pending` | no state yet, `Scheduled` |
| `Running` | `Pending`, `Scheduled`, `Paused` |
| `Paused` | `Running` |
| `Cancelled` | `Cancelling`, `Scheduled`, `Pending`, `Running`, `Paused` |
| `Scheduled`, `Completed`, `Failed`, `Crashed`, `Cancelling` | any non-terminal state except `Cancelling`, which may only become `Cancelled` |

The API answers 409 with the reason (`terminal`, `duplicate`, `invalid-entry`, or `cancelling`) when a transition is rejected; the Python side raises `TransitionRejected`.

## Counters and timing

Each accepted transition updates the run's counters: `failure_count` on `Failed` and on `AwaitingRetry`, `crash_count` on `Crashed`, `start_time` on the first `Running`, and `end_time` and `total_run_time` on a terminal state.

## Events

Each transition that has a meaning outside the run records an event; see the [events catalogue](events.md). `AwaitingRetry`, `AwaitingResource`, and `Cancelling` record none.
