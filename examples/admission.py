# ---
# title: Admission control
# description: Unique keys with debounce, resources keyed by parameter, and runs created for later.
# order: 10
# fixture: served
# ---
#
# Cereyan creates runs from many places at once, so it decides at admission what to
# do with a run that already exists. `Unique` gives a flow a key over its parameters
# and a policy for the second run of the same key: answer with the existing one,
# replace it, debounce it, or throttle it. A resource name can be a template over the
# parameters, so `api:{{ tenant }}` gives each tenant its own slot. And a run can be
# created for later, or with an idempotency key so a repeated request creates nothing.
# This example runs against a server: start `cereyan serve examples/` first.

import time

from cereyan import Unique, client, flow, task


@task(resources={"api:{{ tenant }}": 1})
def call_api(tenant: str) -> str:
    time.sleep(0.5)
    return tenant


# ## Debounce
#
# Ten saves of a document should render it once, with the last version. The run is
# created a second ahead; every further save moves it along and replaces the
# parameters; once it starts, the next save opens a new window.


@flow(unique=Unique(key="{doc}", on_conflict="debounce", debounce=1))
def render(doc: str, version: int) -> str:
    return f"{doc} v{version}"


# ## One slot per tenant
#
# Two runs for `acme` take turns on `api:acme`; a run for `globex` does not wait for
# them. Without a total in `[resources]` each rendered name defaults to one.


@flow
def sync_tenant(tenant: str) -> str:
    return call_api(tenant)


# ## Driving it from a script


def wait_for(run_id: int, predicate, timeout: float = 60.0) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        run = client.get_run(run_id)
        if predicate(run):
            return run
        time.sleep(0.2)
    raise TimeoutError(f"run {run_id} did not reach the expected state")


TERMINAL = ("Completed", "Failed", "Crashed", "Cancelled")

if __name__ == "__main__":
    first = client.run("render", doc="readme", version=1)
    for version in (2, 3):
        moved = client.run("render", doc="readme", version=version)
        assert moved["id"] == first["id"] and moved["conflict"] is True
    done = wait_for(first["id"], lambda r: r["state"]["type"] in TERMINAL)
    print("rendered once with", done["parameters"])

    runs = [client.run("sync_tenant", tenant=t) for t in ("acme", "acme", "globex")]
    finished = [wait_for(r["id"], lambda r: r["state"]["type"] in TERMINAL) for r in runs]
    assert all(r["state"]["type"] == "Completed" for r in finished)
    acme_a, acme_b, globex = finished
    overlap = min(acme_a["end_time"], acme_b["end_time"]) > max(acme_a["start_time"], acme_b["start_time"])
    print("the two acme runs overlapped:", overlap, "· globex started before both ended:", globex["start_time"] < max(acme_a["end_time"], acme_b["end_time"]))

    later = client.run("sync_tenant", tenant="later", delay=1)
    print("created for later:", later["state"]["type"], "at", later["scheduled_time"])
    twice = client.run("sync_tenant", tenant="later", idempotency_key="webhook-42")
    same = client.run("sync_tenant", tenant="later", idempotency_key="webhook-42")
    print("idempotent request answered with the same run:", same["id"] == twice["id"])
    for r in (later, twice):
        wait_for(r["id"], lambda r: r["state"]["type"] in TERMINAL)
