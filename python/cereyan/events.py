"""Events: the catalogue of engine event names, and ``emit_event`` for your own.

``events.run.failed`` is the string ``"run.failed"``, so a name is interchangeable
with the literal everywhere one is accepted — a rule's ``on=``, a comparison
against ``event["name"]``, ``json.dumps``. Each prefix namespace also carries
``any`` for the trailing-wildcard form, so ``events.run.any`` is ``"run.*"``.

A leaf with a dot in it becomes an underscore, since an attribute cannot hold
one: ``events.rule.action_completed`` is ``"rule.action.completed"``.

The catalogue comes from the Rust core, so this module cannot drift from the
names the engine actually emits. Names outside the
``RESERVED_PREFIXES`` are yours: pass any string
to ``emit_event`` and match it in a rule.
"""

from __future__ import annotations

import json

from . import _core, context
from .params import to_json_value


def emit_event(name: str, payload: dict | None = None, resource: dict | None = None) -> None:
    """Record an event. Inside a run the resource defaults to that run; outside a
    run the event goes straight to the store or, when a server is up, to its API.

    Any name is allowed except one under a
    reserved prefix (see ``cereyan.events.RESERVED_PREFIXES``) that the engine does not
    emit, which is always a mistake rather than a custom event.

    Raises:
        ValueError: When the name is empty, not a string, or shadows the
            engine's own namespace.
    """
    if not name or not isinstance(name, str):
        raise ValueError("event name must be a non-empty string")
    # A custom event may not occupy a name the engine owns: without this,
    # `emit_event("run.mine")` would make a mistyped rule appear to work.
    _core.check_event_name(name)
    body = json.dumps(to_json_value(payload or {}))
    run = context.current_run()
    if run is not None and run.backend is not None:
        task = context.current_task_run()
        run.backend.emit_event(name, body, task.id if task is not None else None, resource)
        return
    from . import client as client_module, engine

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


class EventName(str):
    """An engine event name; a ``str`` carrying the exact wire name.

    Attributes:
        resource (str): The resource kind the event hangs off (``run``,
            ``task_run``, ``flow``, ``schedule``, ``resource``, ``rule``).
        when (str): One sentence on when the engine records it.
        payload_fields (tuple[str, ...]): The payload keys the emit site sets.
    """

    resource: str
    when: str
    payload_fields: tuple[str, ...]

    def __new__(cls, name: str, resource: str = "", when: str = "",
                payload_fields: tuple[str, ...] = ()) -> "EventName":
        self = super().__new__(cls, name)
        self.resource = resource
        self.when = when
        self.payload_fields = tuple(payload_fields)
        return self

    def __repr__(self) -> str:
        return f"EventName({str.__str__(self)!r})"


class EventGroup:
    """The events under one reserved prefix, as attributes.

    ``events.run.failed`` is ``"run.failed"`` and ``events.run.any`` is
    ``"run.*"``, which matches every event under the prefix.
    """

    def __init__(self, prefix: str, names: "list[EventName]") -> None:
        self._prefix = prefix
        self._names = tuple(names)
        self.any = EventName(f"{prefix}*", prefix.rstrip("."),
                             f"Every {prefix.rstrip('.')} event")
        for name in names:
            setattr(self, str(name)[len(prefix):].replace(".", "_"), name)

    def __iter__(self):
        return iter(self._names)

    def __repr__(self) -> str:
        return f"EventGroup({self._prefix!r}, {len(self._names)} names)"

    def __getattr__(self, name: str) -> EventName:
        # Only reached for a leaf that is not in the catalogue; the message,
        # with its suggestion, is the whole point of failing here.
        _core.check_event_name(f"{self._prefix}{name.replace('_', '.')}")
        raise AttributeError(name)  # pragma: no cover - the check always raises


RESERVED_PREFIXES: tuple[str, ...] = tuple(_core.reserved_prefixes())
"""Prefixes the engine owns. A name under one of these must be a catalogue
entry; every other name is a custom event and is never checked."""

ALL: tuple[EventName, ...] = tuple(
    EventName(name, resource, when, fields) for name, resource, when, fields in _core.event_names()
)
"""Every event the engine records, in the order the core declares them."""

for _prefix in RESERVED_PREFIXES:
    globals()[_prefix.rstrip(".")] = EventGroup(
        _prefix, [n for n in ALL if n.startswith(_prefix)]
    )
del _prefix

__all__ = ["ALL", "RESERVED_PREFIXES", "EventGroup", "EventName", "emit_event",
           *(p.rstrip(".") for p in RESERVED_PREFIXES)]
