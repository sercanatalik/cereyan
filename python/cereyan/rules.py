"""Code rules: ``@app.rule(on=..., flow=None, tags=None)`` and offline
evaluation of rules for this process's own events."""

from __future__ import annotations

import json
import sys
import time
from dataclasses import dataclass, field
from typing import Any, Callable

from . import _core

_registry: dict[str, "CodeRule"] = {}


@dataclass
class CodeRule:
    """A rule declared in code with ``@app.rule``; its action calls ``fn(event, run)``.

    Re-registered on every server start, shown read-only in the UI, and evaluated on
    the offline path for the process's own events.
    """
    name: str
    fn: Callable
    on: list[str]
    flows: list[str]
    tags: list[str]
    states: list[str]
    project: str | None
    once: str = "per_run"
    cooldown_seconds: float = 0.0
    max_per_minute: int = 60
    allow_self: bool = False
    unless: list[str] = field(default_factory=list)
    within: float | None = None
    at: str | None = None
    tz: str | None = None
    module: str = ""
    source_file: str = ""
    extra_actions: list[dict] = field(default_factory=list)
    _fired_runs: set = field(default_factory=set)
    _recent: list = field(default_factory=list)

    @property
    def callable_name(self) -> str:
        """``module:qualname`` of the rule function, used to dispatch ``call`` actions."""
        return f"{self.module}:{self.fn.__qualname__}"

    @property
    def proactive(self) -> bool:
        """``True`` for a rule with ``unless``, which fires when an expected event does not happen."""
        return bool(self.unless)

    def spec(self) -> dict[str, Any]:
        """The rule as the server stores it: ``when``, ``do``, guards, and the ``unless`` clause."""
        spec = {
            "when": {"events": self.on, "flows": self.flows, "tags": self.tags, "states": self.states, "project": self.project},
            "do": [{"kind": "call", "callable": self.callable_name}, *self.extra_actions],
            "once": self.once,
            "cooldown_seconds": self.cooldown_seconds,
            "max_per_minute": self.max_per_minute,
            "allow_self": self.allow_self,
        }
        if self.unless:
            spec["unless"] = {"events": self.unless, "flows": self.flows, "tags": self.tags, "states": [], "project": self.project}
            if self.within is not None:
                spec["within"] = float(self.within)
            if self.at:
                spec["at"] = {"cron": self.at, "tz": self.tz}
        return spec


def register(app, fn: Callable, on=None, flow=None, tags=None, states=None, project=None,
             unless=None, within=None, at=None, tz=None, **guards) -> CodeRule:
    """Create a `CodeRule` for ``fn`` and register it on ``app``; used by ``App.rule``.

    Args:
        app: The App owning the rule, or ``None`` for a rule outside any App.
        fn: Called as ``fn(event, run)`` when the rule fires.
        on: Event name or names, with ``*`` prefixes such as ``run.*``.
        flow: Flow name or names the rule applies to.
        tags: Run tags the rule requires.
        states: State types the run must be in.
        project: Project scope; defaults to the App's name.
        unless: Expected event name or names for a proactive rule.
        within: Seconds after the ``on`` event by which ``unless`` must happen.
        at: Cron expression for a clock-armed proactive rule.
        tz: IANA timezone for ``at``.
        **guards: ``name``, ``once`` (``"per_run"``), ``cooldown_seconds``,
            ``max_per_minute``, ``allow_self``.

    Raises:
        ValueError: When the combination of ``on``, ``unless``, ``within``, and ``at``
            is not one of the reactive, event-armed, or clock-armed forms.
    """
    if isinstance(on, str):
        on = [on]
    if isinstance(unless, str):
        unless = [unless]
    if unless and not at and (not on or within is None):
        raise ValueError("a rule with unless= needs on= and within= (event-armed) or at= (clock-armed)")
    if at and not unless:
        raise ValueError("at= is only valid together with unless=")
    if at and on:
        raise ValueError("a clock-armed rule (at=) cannot also have on=")
    if not on and not at:
        raise ValueError("a rule needs on= (or at= with unless=)")
    flows = [flow] if isinstance(flow, str) else list(flow or [])
    rule = CodeRule(
        name=guards.pop("name", None) or fn.__name__,
        fn=fn,
        on=list(on or []),
        flows=flows,
        tags=list(tags or []),
        states=list(states or []),
        project=project or (app.name if app is not None else None),
        unless=list(unless or []),
        within=within,
        at=at,
        tz=tz,
        module=fn.__module__ if fn.__module__ != "__main__" else _main_stem(),
        source_file=getattr(sys.modules.get(fn.__module__), "__file__", "") or "",
        **{k: v for k, v in guards.items() if k in ("once", "cooldown_seconds", "max_per_minute", "allow_self")},
    )
    _registry[rule.callable_name] = rule
    if app is not None:
        app.rules.append(rule)
    return rule


def _main_stem():
    import os

    file = getattr(sys.modules.get("__main__"), "__file__", None)
    return os.path.splitext(os.path.basename(file))[0] if file else "__main__"


def all_rules() -> list[CodeRule]:
    """Every code rule registered in this process, in registration order."""
    return list(_registry.values())


def dispatch(callable_name: str, event_json: str, run_json: str) -> str:
    """Called from Rust for `call` actions: run the registered callable."""
    rule = _registry.get(callable_name)
    if rule is None:
        # Resolve module:qualname lazily (the serving process imported the module).
        module, _, qual = callable_name.partition(":")
        import importlib

        obj = importlib.import_module(module)
        for part in qual.split("."):
            obj = getattr(obj, part)
        fn = obj.fn if isinstance(obj, CodeRule) else obj
    else:
        fn = rule.fn
    event = json.loads(event_json)
    run = json.loads(run_json) if run_json and run_json != "null" else None
    result = fn(event, run)
    try:
        return json.dumps(result, default=str)
    except (TypeError, ValueError):
        return "null"


def register_with_store(store: _core.Store) -> list[int]:
    """Re-register code rules on start; returns their row ids."""
    existing = {(r["source"], r["name"], r.get("module")): r for r in json.loads(store.list_rules())}
    ids = []
    for rule in all_rules():
        row = existing.get(("code", rule.name, rule.module))
        rid = store.upsert_rule(rule.name, json.dumps(rule.spec()), "code", rule.module, row["id"] if row else None, row["enabled"] if row else True)
        ids.append(rid)
    store.prune_code_rules(ids)
    return ids


def evaluate_offline(store: _core.Store, event_name: str) -> None:
    """Offline path: evaluate rules against the newest event of this name."""
    page = json.loads(store.query_events(json.dumps({"name": event_name, "limit": 1})))
    if not page["items"]:
        return
    event = page["items"][0]
    run = None
    if event.get("run_id"):
        run_json = store.get_run(event["run_id"])
        run = json.loads(run_json) if run_json else None
    rows = json.loads(store.list_rules())
    run_json = json.dumps(run) if run else "null"
    for row in rows:
        if not row.get("enabled", True):
            continue
        if row.get("unless"):
            _track_offline(row, event, run, run_json)
            continue
        if not _core.rule_matches(json.dumps(row["when"]), json.dumps(event), run_json):
            continue
        _fire_offline(store, row, event, run)


def _track_offline(row: dict, event: dict, run: dict | None, run_json: str) -> None:
    """Arm or disarm an in-process expectation for an event-armed rule."""
    if row.get("at"):
        return  # clock-armed rules need a clock: server only
    key = f"run:{run['id']}" if run else (f"flow:{event['flow_id']}" if event.get("flow_id") else None)
    if key is None:
        return
    slot = (row["id"], key)
    if _core.rule_matches(json.dumps(row["unless"]), json.dumps(event), run_json):
        exp = _expectations.get(slot)
        if exp and event["occurred"] <= exp["deadline"]:
            exp["met"] = True
    if _core.rule_matches(json.dumps(row["when"]), json.dumps(event), run_json) and slot not in _expectations:
        if run and not row.get("allow_self") and run.get("created_by") == f"rule:{row['id']}":
            return
        _expectations[slot] = {"armed_at": event["occurred"], "deadline": event["occurred"] + int(float(row.get("within") or 0) * 1e6),
                               "run_id": run["id"] if run else None, "met": False}


def settle_expectations_offline(store: _core.Store, run_id: int) -> None:
    """At the end of an offline run: fire lapses whose deadline passed unmet."""
    now = int(time.time() * 1e6)
    rows = {r["id"]: r for r in json.loads(store.list_rules())}
    for (rule_id, key), exp in list(_expectations.items()):
        if exp["run_id"] != run_id:
            continue
        del _expectations[(rule_id, key)]
        row = rows.get(rule_id)
        if row is None or not row.get("enabled", True) or exp["met"] or exp["deadline"] > now:
            continue
        run_json = store.get_run(run_id)
        run = json.loads(run_json) if run_json else None
        payload = {"rule_id": rule_id, "rule": row["name"], "flow": run.get("flow_name") if run else None,
                   "project": run.get("project") if run else None, "run": run_id, "run_name": run.get("name") if run else None,
                   "expected": row["unless"].get("events", []), "deadline": exp["deadline"], "armed_at": exp["armed_at"]}
        resource = json.dumps({"kind": "rule", "id": str(rule_id), "name": row["name"]})
        event_id = store.append_event("expectation.lapsed", json.dumps(payload), run_id, None, resource)
        page = json.loads(store.query_events(json.dumps({"name": "expectation.lapsed", "limit": 1})))
        event = page["items"][0] if page["items"] else {"id": event_id, "name": "expectation.lapsed", "payload": payload, "run_id": run_id}
        _fire_offline(store, row, event, run)


def _fire_offline(store: _core.Store, row: dict, event: dict, run: dict | None) -> None:
    now = time.time()
    if run and not row.get("allow_self") and run.get("created_by") == f"rule:{row['id']}":
        return
    guard = _guards.setdefault(row["id"], {"fired": set(), "recent": [], "last": None})
    if row.get("once", "per_run") == "per_run" and run and run["id"] in guard["fired"]:
        return
    if row.get("cooldown_seconds", 0) and guard["last"] and now - guard["last"] < row["cooldown_seconds"]:
        return
    guard["recent"] = [t for t in guard["recent"] if now - t < 60]
    if row.get("max_per_minute", 60) and len(guard["recent"]) >= row["max_per_minute"]:
        return
    guard["recent"].append(now)
    guard["last"] = now
    if run:
        guard["fired"].add(run["id"])
    outcomes = []
    context = json.dumps({"event": event, "run": run, "flow": None, "state": run.get("state") if run else None,
                          "payload": event.get("payload", {}), "parameters": (run or {}).get("parameters", {})})
    for action in row.get("do", []):
        try:
            rendered = json.loads(_core.render_rule_action(json.dumps(action), context))
            detail = _execute_offline(store, rendered, event, run)
            outcomes.append({"kind": action["kind"], "status": "completed", "detail": detail})
        except Exception as exc:  # noqa: BLE001
            outcomes.append({"kind": action["kind"], "status": "failed", "error": f"{type(exc).__name__}: {exc}"})
    store.record_firing(row["id"], event["id"], run["id"] if run else None, json.dumps(outcomes))


_guards: dict[int, dict] = {}
_expectations: dict[tuple[int, str], dict] = {}


def _execute_offline(store: _core.Store, action: dict, event: dict, run: dict | None):
    kind = action["kind"]
    if kind == "call":
        result = dispatch(action.get("callable", ""), json.dumps(event), json.dumps(run) if run else "null")
        return json.loads(result) if result else None
    if kind == "set_state" and run:
        return json.loads(store.transition(run["id"], action["state_type"], None, action.get("message"), None, True))
    if kind == "webhook":
        import urllib.request

        body = (action.get("body") or "").encode()
        headers = {"content-type": "application/json", **{k: str(v) for k, v in (action.get("headers") or {}).items()}}
        req = urllib.request.Request(action["url"], data=body, method=(action.get("method") or "POST").upper(), headers=headers)
        with urllib.request.urlopen(req, timeout=5) as resp:
            return {"status": resp.status}
    if kind == "run_flow":
        return {"deferred": "run_flow actions execute when a server is running"}
    return None
