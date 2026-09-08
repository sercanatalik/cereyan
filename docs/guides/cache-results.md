# How to cache task results

Persist a task's return value and reuse it when the inputs, the code, or both are unchanged. Caching is for values that come back to the flow; for files, use [targets](idempotent-reruns.md).

## Cache by inputs

```python
from cereyan import flow, task, INPUTS

calls = []

@task(cache=INPUTS, persist_result=True)
def lookup(customer: str) -> dict:
    calls.append(customer)
    return {"customer": customer, "tier": "gold"}

@flow
def enrich(customer: str) -> dict:
    return lookup(customer)

assert enrich("acme") == enrich("acme")
assert calls == ["acme"]   # the second call was Cached
enrich("globex")
assert calls == ["acme", "globex"]
```

A hit ends the task run `Cached` with the stored value, records `task_run.cached`, and shows in the UI as such. `persist_result=True` is required: the value has to be stored somewhere to be reused.

## Cache by source too

`cache=INPUTS + SOURCE` also hashes the task's source code, so editing the function invalidates its entries:

```python
from cereyan import flow, task, INPUTS, SOURCE

@task(cache=INPUTS + SOURCE, persist_result=True)
def transform(rows: int) -> int:
    return rows * 2

@flow
def run(rows: int = 3) -> int:
    return transform(rows)

assert run() == 6
```

`cache=SOURCE` alone reuses one result for any inputs while the code is unchanged, which suits parameterless setup steps.

## Expire entries

```python
from datetime import timedelta
from cereyan import flow, task, INPUTS

@task(cache=INPUTS, persist_result=True, cache_expires=timedelta(minutes=30))
def rates(currency: str) -> float:
    return 1.0

@flow
def convert(currency: str = "EUR") -> float:
    return rates(currency)

assert convert() == 1.0
```

An entry older than `cache_expires` is a miss and the task runs again.

## Serialisation

Results are pickled by default. Pass `serializer="json"` to store JSON instead when the value is plain data and you want to read it from other tools or the UI. Entries record the Python version that wrote them; a different minor version is a miss.

## Where entries live and how to clear them

Entries are files under `<home>/storage/`, keyed by a hash of the task key plus the inputs and source. Retention never touches them. Delete the directory, or the files for one task, to clear the cache; a missing entry is a miss.

## Replay after a pause

Tasks with `cache=INPUTS` are what make [pausing for approval](human-approval.md) cheap: the resumed attempt replays the flow from the top and the cached tasks return at once.

Related: [Targets, caching and results](../concepts/targets-caching-results.md).
