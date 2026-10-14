# How to run a side effect once

A task that charges a card, sends an email or writes to another system has an effect outside cereyan, and cereyan runs tasks at least once. A crash rerun executes again any task that had not finished, and so does a task whose completion was still on its way to the server when the engine died. Two tools make that safe: an **effect key** for APIs that deduplicate, and **`once` tasks** for effects that cannot.

## Pass the effect key to an API that deduplicates

Every task call has an effect key, `cereyan.runtime.task_run.effect_key`. It identifies one logical attempt at the call. The same call has the same key on a later pass of the run, in its crash rerun, and in a retry from failure. A retry from the start, a forced run, and any other call each get a key of their own. Give it to any API that accepts an idempotency key, and a repeat becomes harmless:

```python
from cereyan import flow, runtime, task

charges: dict[str, str] = {}   # stands in for a payment API that deduplicates

def create_charge(amount: int, idempotency_key: str) -> str:
    return charges.setdefault(idempotency_key, f"ch_{len(charges) + 1}")

@task
def charge(amount: int) -> str:
    return create_charge(amount, idempotency_key=runtime.task_run.effect_key)

@flow
def checkout() -> str:
    return charge(4200)

assert checkout().startswith("ch_")
```

Most payment, email and storage APIs accept such a key. Prefer this to `once`: it costs nothing, and the API is the one that knows whether the effect happened. The task run page shows the key, so you can find the call in the other system's logs.

## Mark a task `once` when the effect cannot take a key

`@task(once=True)` says the task's effect must not repeat silently:

- **Its start and end are reported to the server synchronously.** The run does not move on until the server has them. Other tasks are reported in batches every 100 ms, which is why a task that completed just before a crash can run again. A `once` task completed just before a crash is replayed from its checkpoint instead.
- **An interrupted call is in doubt.** When a crash rerun, or a retry from failure, reaches a `once` call that an earlier run started and never finished, the call does not execute again. Nobody knows whether its effect happened, so something has to settle it: the task's probe, or a person.

```python
from cereyan import flow, task

sent: list[str] = []

@task(once=True)
def send_invoice(customer: str) -> str:
    sent.append(customer)
    return f"invoice for {customer}"

@flow
def bill() -> str:
    return send_invoice("acme")

assert bill() == "invoice for acme" and sent == ["acme"]
```

Each `once` call costs two loopback round trips on a server, and nothing extra offline, where every report is already written before the run moves on.

## Settle an interrupted call with a probe

A probe looks in the outside world for the effect. It takes the task's arguments and runs as part of the task run, so it can read `runtime.task_run.effect_key`. It returns the task's result when the effect happened, or `NOT_DONE` when it did not:

```python
from cereyan import NOT_DONE, flow, runtime, task

uploaded: dict[str, str] = {}   # stands in for an object store

def find_upload(name: str):
    return uploaded.get(runtime.task_run.effect_key, NOT_DONE)

@task(once=True, probe=find_upload)
def upload(name: str) -> str:
    uploaded[runtime.task_run.effect_key] = f"s3://reports/{name}"
    return uploaded[runtime.task_run.effect_key]

@flow
def publish() -> str:
    return upload("q3.pdf")

assert publish() == "s3://reports/q3.pdf"
```

When the probe finds the effect, the task run completes as `Recovered` with the probe's value, and a `task_run.recovered` event is recorded. When it returns `NOT_DONE`, the task executes. If the probe raises, the call is settled as though there were no probe.

## Answer an in-doubt question

Without a probe, the run pauses as `Paused/InDoubt` and the question appears in the inbox and on the run page: *"charge-0 (charge) was interrupted in an earlier run and may have taken effect. Did its effect happen?"* Answer it from the form, `POST /api/runs/{id}/resume`, `Client.resume` or the MCP `resume_run` tool, as you would answer [`wait_for_input`](human-approval.md):

<!-- notest: needs a server and a paused run -->
```{.python notest}
from cereyan.client import default_client

c = default_client()
c.resume(run_id, {"action": "happened", "result": "ch_1"})   # the task returns "ch_1"
c.resume(run_id, {"action": "run_again"})                    # the task executes
c.resume(run_id, {"action": "fail"})                         # the task fails with TaskInDoubt
```

The answer is kept with the run's other answers, so a later crash does not ask again. A call reached off the flow body's thread, such as a `once` task submitted from inside another task, cannot pause the run. Without a probe it fails with `TaskInDoubt`. A task submitted from the flow body with `submit` or `map` can still ask.

## Retry a `once` task

An attempt that raised, after a timeout for instance, may still have had its effect. So a `once` task with `retries` needs a probe, and the decorator refuses one without. Before each retry the probe is asked first:

```python
from cereyan import NOT_DONE, flow, task

charged: list[int] = []

def already_charged(amount: int):
    return "ch_1" if charged else NOT_DONE

@task(once=True, retries=2, retry_delay=0, probe=already_charged)
def charge(amount: int) -> str:
    charged.append(amount)
    raise TimeoutError("the charge went through, but the response never came")

@flow
def checkout() -> str:
    return charge(4200)

assert checkout() == "ch_1" and charged == [4200]
```

## What `once` cannot do

`once` stops cereyan from repeating an effect silently, but a crash still leaves the outcome unknown. A worker cut off from the network keeps executing the call even after its run is declared crashed, and its reports are refused. The rerun reports the call as in doubt, which is the honest answer. Undoing an effect is not built in: if the right answer is to reverse it, do that in the task or flow that follows.

Tools of a durable Pydantic AI agent get the same treatment through `once_tools`; see [Tools that must not repeat](durable-pydantic-ai-agent.md#tools-that-must-not-repeat).

Related: [Retry, time out and survive crashes](retries-timeouts-crashes.md), [Make reruns idempotent with targets](idempotent-reruns.md), [Pause a run for approval](human-approval.md), [Runs and states](../concepts/runs-and-states.md#checkpoints).
