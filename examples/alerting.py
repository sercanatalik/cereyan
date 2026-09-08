# ---
# title: Alerting
# description: A custom event, a code rule that reacts to it, and a proactive rule for a run that overruns.
# order: 5
# fixture: served
# ---
#
# Rules turn events into actions. This module declares a flow that emits a custom
# event when a table looks empty, a code rule that reacts to that event, and a
# proactive rule that fires when a run of the flow does not complete within ten
# minutes of starting. Rules fire in the server, so run this against
# `cereyan serve examples/`.

import time

from cereyan import App, client, emit_event, get_run_logger

app = App("shop")


# ## A flow that emits an event


@app.flow
def check_orders(count: int = 0) -> int:
    if count == 0:
        emit_event("orders.table_empty", {"table": "orders"})
    get_run_logger().info("orders: %d", count)
    return count


# ## A reactive code rule
#
# `on` accepts a prefix; the function receives the event and the run as dicts and
# runs as the rule's `call` action.


@app.rule(on="orders.*", flow="check_orders", once="per_run")
def alert_empty(event, run):
    print(f"ALERT {event['payload']['table']} is empty (run {run['name']})")
    return {"alerted": True}


# ## A proactive rule
#
# Armed when a `check_orders` run starts, disarmed by its completion, fired by the
# server if two minutes pass first.


@app.rule(on="run.running", flow="check_orders", unless="run.completed", within=120)
def check_orders_overran(event, run):
    print(f"ALERT {run['name']} has been running for two minutes")


# ## Run it and read the firing

if __name__ == "__main__":
    run = client.run("check_orders", count=0)
    deadline = time.time() + 30
    while time.time() < deadline and client.get_run(run["id"])["state"]["type"] not in ("Completed", "Failed"):
        time.sleep(0.1)
    api = client.default_client()
    events = api.events(kind="orders.*", run_id=run["id"])
    assert events and events[0]["name"] == "orders.table_empty"
    # Rules fire after the run that emitted the event has finished, not with it,
    # so wait for the firing rather than reading the count straight away.
    deadline = time.time() + 30
    while time.time() < deadline:
        fired = [r for r in api.rules() if r["name"] == "alert_empty"]
        if fired and fired[0]["fire_count"] >= 1:
            break
        time.sleep(0.1)
    assert fired and fired[0]["fire_count"] >= 1, fired
    print("rule fired", fired[0]["fire_count"], "time(s)")
