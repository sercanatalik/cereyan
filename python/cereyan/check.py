"""``cereyan check``: import a directory the way ``serve`` does and report what
would stop it from serving cleanly, without opening the store.

The report is one dict, the same object ``cereyan check --json`` prints, so the
MCP tool that arrives later can serialise it unchanged.
"""

from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from typing import Any
from zoneinfo import ZoneInfo

from . import _core, apps
from .config import resource_totals
from .exceptions import CereyanError

PREVIEW_FIRES = 3


def _finding(level: str, kind: str, message: str, *, module: str | None = None,
             flow: str | None = None, detail: str | None = None) -> dict[str, Any]:
    out: dict[str, Any] = {"level": level, "kind": kind, "message": message}
    if module is not None:
        out["module"] = module
    if flow is not None:
        out["flow"] = flow
    if detail is not None:
        out["detail"] = detail
    return out


def _last_line(traceback_text: str) -> str:
    lines = [line for line in traceback_text.strip().splitlines() if line.strip()]
    return lines[-1] if lines else "import failed"


def _iso(micros: int, zone: str | None) -> str:
    try:
        tz = ZoneInfo(zone) if zone else timezone.utc
    except Exception:
        tz = timezone.utc
    return datetime.fromtimestamp(micros / 1_000_000, tz).isoformat()


def _describe_schedule(spec: dict[str, Any]) -> str:
    kind = spec.get("kind")
    if kind == "cron":
        return f"cron {spec.get('cron')}"
    if kind == "interval":
        return f"every {spec.get('interval')} s"
    if kind == "rrule":
        return f"rrule {spec.get('rrule')}"
    return str(kind)


def reference_micros(now: datetime | str | None) -> int:
    """``now`` as microseconds UTC: a datetime, an ISO 8601 string (naive means UTC), or the clock."""
    if now is None:
        return _core.now_micros()
    if isinstance(now, str):
        try:
            now = datetime.fromisoformat(now)
        except ValueError:
            raise CereyanError(f"invalid --now {now!r}: expected an ISO 8601 instant such as 2026-09-20T06:00:00Z") from None
    if now.tzinfo is None:
        now = now.replace(tzinfo=timezone.utc)
    return int(now.timestamp() * 1_000_000)


def _resource_declared(name: str, totals: dict) -> bool:
    """Whether a resource name, possibly templated (`api:{{ tenant }}`), has a
    total: its own, or a pattern total such as `api:*` that matches it once
    every placeholder is read as a wildcard."""
    import re
    from fnmatch import fnmatchcase

    if name in totals:
        return True
    wild = re.sub(r"\{\{[^}]*\}\}|\{[^}]*\}", "*", name)
    return any("*" in key and (fnmatchcase(wild, key) or fnmatchcase(key, wild)) for key in totals)


def check_directory(directory: str, now: datetime | str | None = None) -> dict[str, Any]:
    """Import ``directory`` as ``cereyan serve`` would and report on it.

    Raises `CereyanError` when the directory cannot be checked at all. Never opens
    the store or writes to the runtime home.
    """
    from . import engine
    from .serve import discover_modules, import_modules

    directory = os.path.abspath(directory)
    if not os.path.isdir(directory):
        raise CereyanError(f"{directory} is not a directory")
    at = reference_micros(now)
    findings: list[dict[str, Any]] = []

    known_apps = {id(a) for a in apps.all_apps()}
    modules = discover_modules(directory)
    engine.runner.suppress_top_level_runs(True, "cereyan check is importing modules")
    try:
        failures = import_modules(directory, modules)
    finally:
        engine.runner.suppress_top_level_runs(False)
    failed = {name for name, _ in failures}
    for name, tb in failures:
        findings.append(_finding("error", "import", _last_line(tb), module=name, detail=tb))

    registered = [a for a in apps.all_apps() if id(a) not in known_apps] or apps.all_apps()
    flows = [f for app in registered for f in app.flows.values()]
    routes = [r for app in registered for r in app.routes]
    rules = [r for app in registered for r in app.rules]
    totals = resource_totals(directory)

    flow_reports: list[dict[str, Any]] = []
    for f in flows:
        label = f"{f.project}/{f.name}"
        own: list[dict[str, Any]] = []
        if f.after:
            names = f.after.get("flows") or [f.after["flow"]]
            unknown = [n for n in names if not any(o.project == f.project and o.name == n for o in flows)]
            if unknown:
                own.append(_finding("error", "upstream",
                                    "unknown upstream flow " + ", ".join(f"'{n}'" for n in unknown), flow=label))
        previews: list[dict[str, Any]] = []
        for spec in f.schedules:
            key = spec.get("key")
            entry: dict[str, Any] = {"key": key, "kind": spec.get("kind"), "next": []}
            try:
                fires = _core.schedule_fires(json.dumps(spec), at, PREVIEW_FIRES)
            except ValueError as exc:
                own.append(_finding("error", "schedule", f"schedule {key} ({_describe_schedule(spec)}): {exc}", flow=label))
            else:
                entry["next"] = [_iso(m, spec.get("timezone")) for m in fires]
                own.append(_finding("info", "schedule",
                                    f"schedule {key} ({_describe_schedule(spec)}): next "
                                    + (", ".join(entry["next"]) or "never"), flow=label))
            previews.append(entry)
        for name in sorted(f.resources):
            if not _resource_declared(name, totals):
                own.append(_finding("warning", "resource",
                                    f"resource '{name}' is not in [resources] of cereyan.toml", flow=label))
        findings.extend(own)
        flow_reports.append({"project": f.project, "name": f.name, "schedules": previews, "findings": own})

    specs = [
        {"id": i, "method": r.method, "path": r.path, "source": f"{r.handler.__module__}:{r.handler.__qualname__}"}
        for i, r in enumerate(routes)
    ]
    if specs:
        try:
            _core.check_routes(json.dumps(specs))
        except ValueError as exc:
            findings.append(_finding("error", "route", str(exc)))

    errors = sum(1 for x in findings if x["level"] == "error")
    warnings = sum(1 for x in findings if x["level"] == "warning")
    return {
        "ok": errors == 0,
        "errors": errors,
        "warnings": warnings,
        "directory": directory,
        "now": _iso(at, None),
        "modules": [m for m in modules if m not in failed],
        "flows": flow_reports,
        "routes": [{"method": s["method"], "path": s["path"], "source": s["source"]} for s in specs],
        "rules": len(rules),
        "findings": findings,
    }


def render(report: dict[str, Any]) -> str:
    """The human form: findings grouped by module and flow, then one summary line."""
    lines: list[str] = []
    for x in report["findings"]:
        if x["kind"] == "import":
            lines.append(f"{x['module']}: import error: {x['message']}")
    for flow in report["flows"]:
        for x in flow["findings"]:
            prefix = {"error": "error", "warning": "warning", "info": ""}[x["level"]]
            lines.append(f"{x['flow']}: {prefix + ': ' if prefix else ''}{x['message']}")
    for x in report["findings"]:
        if x["kind"] == "route":
            lines.append(f"routes: error: {x['message']}")
    schedules = sum(len(f["schedules"]) for f in report["flows"])
    lines.append(
        f"checked {len(report['modules'])} module(s), {len(report['flows'])} flow(s), {len(report['routes'])} route(s), "
        f"{report['rules']} rule(s), {schedules} schedule(s) at {report['now']}: "
        f"{report['errors']} error(s), {report['warnings']} warning(s)"
    )
    return "\n".join(lines)
