# ---
# title: Freshness and deadlines
# description: Health per flow from its runs, a rule that pages when a run overruns, and a backfill of just the missing days.
# order: 11
# fixture: served
# ---
#
# A flow can say how fresh it must be and how long it should take. The server derives
# PASS, WARN or FAIL from the runs it already records, shows it in the Health column
# of the Flows page, and records `run.overdue` once when a run outlasts its
# expectation, which a rule can turn into a page. This example runs against a server:
# start `cereyan serve examples/` first.

import time
from datetime import date, timedelta

from cereyan import App, client, flow, task

app = App("ops")


@task
def load(day: date) -> str:
    return str(day)


@app.flow(fresh_within=timedelta(hours=26), expect_by="0 9 * * *", expect_by_tz="UTC")
def nightly(day: date = date(2026, 3, 1)) -> str:
    return load(day)


@app.flow(expected_duration=1)
def usually_quick(seconds: float = 0.1) -> None:
    time.sleep(seconds)


# ## Paging on an overrun
#
# `run.overdue` is an engine event like any other, so a code rule on it does what a
# rule on `run.failed` would: here it just prints, where you would send a webhook.


@app.rule(on="run.overdue")
def overran(event, run):
    print(f"run {run['id']} of {run['flow_name']} is overdue by {event['payload']['elapsed_seconds']:.0f}s")


# ## Driving it from a script
#
# `nightly` is healthy once a run completes. `usually_quick` with a long sleep goes
# WARN while the run overruns and the rule fires once. The backfill at the end asks
# for three days but only makes the two that never completed.


def wait_for(run_id: int, predicate, timeout: float = 60.0) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        run = client.get_run(run_id)
        if predicate(run):
            return run
        time.sleep(0.2)
    raise TimeoutError(f"run {run_id} did not reach the expected state")


def health_of(name: str) -> dict:
    return next(f["health"] for f in client.list_flows() if f["name"] == name and f["project"] == "ops")


TERMINAL = ("Completed", "Failed", "Crashed", "Cancelled")

if __name__ == "__main__":
    api = client.default_client()
    run = client.run("nightly", project="ops", day="2026-03-01")
    wait_for(run["id"], lambda r: r["state"]["type"] in TERMINAL)
    print("nightly:", health_of("nightly")["status"])

    slow = client.run("usually_quick", project="ops", seconds=8)
    deadline = time.time() + 40
    while time.time() < deadline and not any(e["run_id"] == slow["id"] for e in api.events(kind="run.overdue")):
        time.sleep(0.5)
    print("usually_quick while overrunning:", health_of("usually_quick")["status"], health_of("usually_quick")["reasons"])
    wait_for(slow["id"], lambda r: r["state"]["type"] in TERMINAL)

    flow_id = next(f["id"] for f in client.list_flows() if f["name"] == "nightly" and f["project"] == "ops")
    backfill = api.backfill(flow_id, "day", values=["2026-03-01", "2026-03-02", "2026-03-03"], missing_only=True)
    print("backfill made", backfill["total"], "runs for the days that had none")
    assert backfill["total"] == 2
