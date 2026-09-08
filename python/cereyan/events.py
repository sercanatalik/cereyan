"""Custom events: ``emit_event(name, payload=None, resource=None)``."""

from __future__ import annotations

import json

from . import context
from .params import to_json_value


def emit_event(name: str, payload: dict | None = None, resource: dict | None = None) -> None:
    """Record an event. Inside a run the resource defaults to that run; outside a
    run the event goes straight to the store or, when a server is up, to its API."""
    if not name or not isinstance(name, str):
        raise ValueError("event name must be a non-empty string")
    body = json.dumps(to_json_value(payload or {}))
    run = context.current_run()
    if run is not None and run.backend is not None:
        task = context.current_task_run()
        run.backend.emit_event(name, body, task.id if task is not None else None, resource)
        return
    from . import _core, client as client_module, engine

    try:
        store = engine.get_store()
    except _core.StoreLocked:
        server = client_module.find_server(engine.resolved_home())
        if server is None:
            raise
        server._request("POST", "/api/events", body={"name": name, "payload": json.loads(body), "resource": resource})
        return
    store.append_event(name, body, None, None, json.dumps(resource) if resource else None)
    # Offline code rules see the event immediately.
    from .rules import evaluate_offline

    evaluate_offline(store, name)
