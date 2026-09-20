"""The ``cereyan`` command: ``run``, ``serve``, ``check``, ``backfill``, ``mcp``, and ``runs ls``."""

from __future__ import annotations

import argparse
import importlib
import importlib.util
import json
import logging
import os
import sys
from collections import Counter
from datetime import datetime, timezone
from typing import Any

from . import engine
from .client import AuthRequired
from .exceptions import CereyanError, ParameterError

EXIT_OK = 0
EXIT_FAILED = 1
EXIT_NOTHING_RAN = 2
EXIT_SCHEDULING = 3


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="cereyan", description="Local-first pipeline orchestrator")
    parser.add_argument("--home", help="runtime home directory (default: $CEREYAN_HOME or ~/.cereyan)")
    parser.add_argument("--token", dest="client_token", help="API token for a running server (also CEREYAN_TOKEN)")
    sub = parser.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="run a flow offline")
    run.add_argument("target", help="module_or_file:flow, e.g. pipeline.py:etl")
    run.add_argument("--param", "-p", action="append", default=[], metavar="NAME=VALUE", help="set a flow parameter; repeatable, values are coerced from the flow's type hints")
    run.add_argument("--quiet", "-q", action="store_true", help="do not echo run logs")
    run.add_argument("--at", help="instead of running now, create the run on the running server for this ISO 8601 time (naive means UTC)")
    run.add_argument("--in", dest="delay", metavar="DURATION", help="instead of running now, create the run on the running server this long from now: seconds, or 10m, 2h, 1d")
    run.add_argument("--json", action="store_true", help="with --at or --in, print the created run as JSON")
    run.set_defaults(func=cmd_run)

    serve = sub.add_parser("serve", help="serve flows with the API, UI, and engines")
    serve.add_argument("dir", nargs="?", help="directory to import flows from (default: current)")
    serve.add_argument("--host", help="bind address (also CEREYAN_HOST or [server] host; default 127.0.0.1)")
    serve.add_argument("--port", type=int, help="TCP port (also CEREYAN_PORT or [server] port; default 4200, 0 picks a free port)")
    serve.add_argument("--max-engines", type=int, help="size of the warm engine pool (also [server] max_engines)")
    serve.add_argument("--engine-max-runs", type=int, help="recycle an engine after this many runs (also [server] engine_max_runs)")
    serve.add_argument("--no-open", action="store_true", help="do not open the browser")
    serve.add_argument("--crash-retries", type=int, help="default crash retry limit (flow decorators override)")
    serve.add_argument("--token", help="require this API token (also CEREYAN_TOKEN or [server] token)")
    serve.add_argument("--socket", help="also listen on this Unix socket path (also CEREYAN_SOCKET or [server] socket)")
    serve.add_argument("--base-path", help="serve everything under this URL path, e.g. /cereyan (also CEREYAN_BASE_PATH or [server] base_path; default: the root)")
    serve.add_argument("--enable-auth", action="store_true", default=None, help="validate credentials with the registered @app.authenticator (also CEREYAN_ENABLE_AUTH or [server] enable_auth)")
    serve.add_argument("--auth-cookie", help="cookie the authenticator reads the credential from when there is no bearer header (also CEREYAN_AUTH_COOKIE or [server] auth_cookie)")
    serve.add_argument("--auth-scope", help="api checks /api/* and /mcp; all also checks the UI and custom routes, and needs --enable-auth (also CEREYAN_AUTH_SCOPE or [server] auth_scope; default api)")
    serve.add_argument("--login-url", help="sign-in page linked from 401 responses and the UI (also CEREYAN_LOGIN_URL or [server] login_url)")
    serve.add_argument("--allowed-host", action="append", dest="allowed_hosts", metavar="HOST", help="also answer to this host name and accept browser pages from it; repeat for more (also CEREYAN_ALLOWED_HOSTS, comma-separated, or [server] allowed_hosts)")
    serve.add_argument("--allow-unauthenticated", action="store_true", default=None, help="serve without a token when bound beyond loopback instead of generating one into <home>/token (also CEREYAN_ALLOW_UNAUTHENTICATED or [server] allow_unauthenticated)")
    serve.add_argument("--mcp-read-only", action="store_true", default=None, help="list and allow only MCP tools that change nothing (also CEREYAN_MCP_READ_ONLY or [server] mcp_read_only)")
    serve.add_argument("--metrics-public", action="store_true", default=None, help="serve /api/metrics without a token, as /api/health (also CEREYAN_METRICS_PUBLIC or [server] metrics_public)")
    serve.add_argument("--public-url", help="the address people reach the UI at, used for run links in rule templates (also CEREYAN_PUBLIC_URL or [server] public_url)")
    serve.set_defaults(func=cmd_serve)

    check = sub.add_parser("check", help="import a directory as serve would and report problems, without touching the store")
    check.add_argument("dir", nargs="?", help="directory to check (default: current directory)")
    check.add_argument("--json", action="store_true", help="print the report as one JSON object")
    check.add_argument("--strict", action="store_true", help="treat warnings as errors")
    check.add_argument("--now", help="reference instant for schedule previews, ISO 8601 (default: now; naive means UTC)")
    check.set_defaults(func=cmd_check)

    backup = sub.add_parser("backup", help="write a consistent copy of the store to <home>/backups/ (through the running server, or directly)")
    backup.add_argument("--json", action="store_true", help="print the copy's path as JSON")
    backup.set_defaults(func=cmd_backup)

    backfill = sub.add_parser("backfill", help="create runs over a date range (requires a running server)")
    backfill.add_argument("flow", help="flow name, optionally project/flow")
    backfill.add_argument("--param", required=True, help="date or datetime parameter name")
    backfill.add_argument("--start", required=True, help="first value of the parameter, a date or datetime")
    backfill.add_argument("--end", required=True, help="last value of the parameter, inclusive")
    backfill.add_argument("--interval", default="1d", help="seconds or a duration like 1d, 12h (default 1d)")
    backfill.add_argument("--concurrency", type=int, default=1, help="how many of the backfill's runs may execute at once")
    backfill.add_argument("--reverse", action="store_true", help="create the newest value first")
    backfill.add_argument("--extra", action="append", default=[], metavar="NAME=VALUE", help="fixed value for another flow parameter; repeatable")
    backfill.add_argument("--json", action="store_true", help="print the backfill status as JSON")
    backfill.set_defaults(func=cmd_backfill)

    pause = sub.add_parser("pause", help="pause every schedule at once (requires a running server); running runs continue")
    pause.add_argument("--reason", help="shown in the UI banner and recorded on the event")
    pause.add_argument("--until", help="resume on its own at this ISO 8601 time (naive means UTC)")
    pause.add_argument("--suppress-rules", action="store_true", help="record rules that would fire as suppressed instead of acting")
    pause.add_argument("--json", action="store_true", help="print the scheduler status as JSON")
    pause.set_defaults(func=cmd_pause)

    resume = sub.add_parser("resume", help="end the global pause: held runs start and schedules catch up (requires a running server)")
    resume.add_argument("--json", action="store_true", help="print the scheduler status as JSON")
    resume.set_defaults(func=cmd_resume)

    mcp = sub.add_parser("mcp", help="MCP server over stdio for agent hosts (proxies to the running server)")
    mcp.add_argument("--url", help="server URL (default: from server.json)")
    mcp.add_argument("--socket", help="Unix socket path of the server")
    mcp.set_defaults(func=cmd_mcp)

    runs = sub.add_parser("runs", help="inspect runs")
    runs_sub = runs.add_subparsers(dest="runs_command", required=True)
    ls = runs_sub.add_parser("ls", help="list recent runs")
    ls.add_argument("--flow", help="only runs of this flow name")
    ls.add_argument("--project", help="only runs of flows in this project")
    ls.add_argument("--group", help="only runs of flows in this group, declared or defaulted to the project")
    ls.add_argument("--state", help="state type, e.g. Failed")
    ls.add_argument("--limit", type=int, default=20, help="number of runs to show")
    ls.add_argument("--json", action="store_true", help="print the runs as JSON")
    ls.set_defaults(func=cmd_runs_ls)
    compare = runs_sub.add_parser("compare", help="what changed between two runs (requires a running server)")
    compare.add_argument("baseline", type=int, help="run id of the baseline, usually the last good run")
    compare.add_argument("other", type=int, help="run id of the run in question")
    compare.add_argument("--json", action="store_true", help="print the comparison as JSON")
    compare.set_defaults(func=cmd_runs_compare)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    engine.configure(args.home)
    if getattr(args, "client_token", None):
        os.environ["CEREYAN_TOKEN"] = args.client_token
    try:
        return int(args.func(args) or 0)
    except AuthRequired as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    finally:
        engine.close_store()


# ---------------------------------------------------------------------------
# run


def load_target(target: str):
    """Resolve ``module_or_file:flow`` to a Flow, or raise CereyanError listing flows."""
    if ":" not in target:
        raise CereyanError(f"target {target!r} must look like module_or_file:flow")
    module_ref, flow_name = target.rsplit(":", 1)
    from .engine.runner import suppress_top_level_runs

    suppress_top_level_runs(True, "cereyan run is loading the module")
    try:
        module = _import(module_ref)
    finally:
        suppress_top_level_runs(False)
    from .flows import Flow

    flows = {v.name: v for v in vars(module).values() if isinstance(v, Flow)}
    flow = flows.get(flow_name)
    if flow is None:
        available = ", ".join(sorted(flows)) or "none"
        raise CereyanError(f"flow {flow_name!r} not found in {module_ref}; defined flows: {available}")
    return flow


def _import(module_ref: str):
    if module_ref.endswith(".py") or os.path.sep in module_ref or os.path.isfile(module_ref):
        path = os.path.abspath(module_ref)
        if not os.path.isfile(path):
            raise CereyanError(f"file {module_ref!r} does not exist")
        directory = os.path.dirname(path)
        if directory not in sys.path:
            sys.path.insert(0, directory)
        name = os.path.splitext(os.path.basename(path))[0]
        spec = importlib.util.spec_from_file_location(name, path)
        if spec is None or spec.loader is None:
            raise CereyanError(f"cannot load {module_ref!r}")
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
        return module
    if os.getcwd() not in sys.path:
        sys.path.insert(0, os.getcwd())
    try:
        return importlib.import_module(module_ref)
    except ImportError as exc:
        raise CereyanError(f"cannot import {module_ref!r}: {exc}") from exc


def parse_params(items: list[str]) -> dict[str, str]:
    out: dict[str, str] = {}
    for item in items:
        if "=" not in item:
            raise CereyanError(f"--param {item!r} must look like NAME=VALUE")
        key, value = item.split("=", 1)
        out[key.strip()] = value
    return out


def cmd_serve(args) -> int:
    from .serve import serve

    try:
        return serve(
            args.dir,
            host=args.host,
            port=args.port,
            max_engines=args.max_engines,
            engine_max_runs=args.engine_max_runs,
            open_browser=False if args.no_open else None,
            crash_retries=args.crash_retries,
            token=args.token,
            socket=args.socket,
            base_path=args.base_path,
            enable_auth=args.enable_auth,
            auth_cookie=args.auth_cookie,
            auth_scope=args.auth_scope,
            login_url=args.login_url,
            allowed_hosts=args.allowed_hosts,
            allow_unauthenticated=args.allow_unauthenticated,
            mcp_read_only=args.mcp_read_only,
            metrics_public=args.metrics_public,
            public_url=args.public_url,
        )
    except CereyanError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING


def cmd_check(args) -> int:
    from .check import check_directory, render

    try:
        report = check_directory(args.dir or os.getcwd(), now=args.now)
    except CereyanError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    failed = report["errors"] > 0 or (args.strict and report["warnings"] > 0)
    if args.strict:
        report["ok"] = not failed
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        print(render(report))
    return EXIT_FAILED if failed else EXIT_OK


def cmd_backup(args) -> int:
    from . import client as client_module

    try:
        server = client_module.find_server(engine.resolved_home())
        if server is not None:
            path = server._request("POST", "/api/database/backup")["path"]
        else:
            path = engine.get_store().backup()
    except (client_module.ApiError, RuntimeError, OSError) as exc:
        print(f"error: could not write the copy: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    print(json.dumps({"path": path}) if args.json else path)
    return EXIT_OK


def parse_duration(text: str) -> float:
    """Seconds from ``90``, ``90s``, ``10m``, ``2h``, or ``1d``."""
    t = text.strip().lower()
    units = {"s": 1, "m": 60, "h": 3600, "d": 86_400}
    scale = units.get(t[-1:], None)
    number = t[:-1] if scale else t
    try:
        seconds = float(number) * (scale or 1)
    except ValueError:
        raise ValueError(f"cannot read {text!r} as a duration (use seconds, or 10m, 2h, 1d)") from None
    if seconds < 0:
        raise ValueError("a duration cannot be negative")
    return seconds


def _run_later(args, flow, values) -> int:
    from .client import _micros
    from .engine.runner import submit_later

    try:
        if args.at and args.delay:
            raise ValueError("give --at or --in, not both")
        if args.at:
            starts = _micros(args.at)
        else:
            starts = int(datetime.now(timezone.utc).timestamp() * 1_000_000) + int(parse_duration(args.delay) * 1_000_000)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    try:
        run = submit_later(flow, values, starts)
    except (CereyanError, AuthRequired) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    if args.json:
        print(json.dumps(run, indent=2))
    else:
        when = datetime.fromtimestamp((run.get("scheduled_time") or starts) / 1_000_000, tz=timezone.utc).isoformat()
        print(f"run {run['id']} ({run['name']}) scheduled for {when}")
    return EXIT_OK


def cmd_run(args) -> int:
    if not args.quiet:
        logging.basicConfig(
            level=logging.INFO, format="%(asctime)s %(levelname)-7s %(message)s", stream=sys.stderr
        )
    try:
        flow = load_target(args.target)
        values = flow.coerce(parse_params(args.param))
    except ParameterError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    except CereyanError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    except Exception as exc:  # import-time failure inside user code
        print(f"error: loading {args.target!r} failed: {type(exc).__name__}: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING

    if args.at or args.delay:
        return _run_later(args, flow, values)

    from .engine.runner import RunFailed

    try:
        flow(**values)
    except RunFailed:
        pass  # outcome recorded; summary decides the exit code
    except CereyanError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
    except BaseException:
        pass

    outcome = engine.last_outcome()
    if not outcome or not outcome.get("run"):
        print("nothing ran", file=sys.stderr)
        return EXIT_NOTHING_RAN
    return print_summary(outcome["run"], outcome["tasks"])


def _fmt_duration(micros: int | None) -> str:
    if micros is None:
        return "-"
    seconds = micros / 1_000_000
    if seconds < 1:
        return f"{seconds * 1000:.0f} ms"
    if seconds < 60:
        return f"{seconds:.2f} s"
    minutes, sec = divmod(seconds, 60)
    return f"{int(minutes)} min {sec:.0f} s"


def print_summary(run: dict, tasks: list[dict]) -> int:
    counts = Counter(t["state"]["name"] for t in tasks)
    state = run["state"]
    lines = ["", "===== Cereyan execution summary =====", ""]
    lines.append(f"Run:      {run['name']}  ({run['project']}/{run['flow_name']})")
    lines.append(f"State:    {state['name']}" + (f"  {state['message']}" if state.get("message") else ""))
    lines.append(f"Duration: {_fmt_duration(run.get('total_run_time'))}")
    lines.append("")
    if tasks:
        lines.append("Task runs:")
        for name, n in sorted(counts.items()):
            lines.append(f"  {n:>4}  {name}")
    else:
        lines.append("Task runs: none")
    lines.append("")
    if state["type"] == "Completed":
        verdict, code = "run completed", EXIT_OK
    elif state["type"] == "Failed":
        verdict, code = "run failed", EXIT_FAILED
    else:
        verdict, code = f"run ended {state['name'].lower()}", EXIT_NOTHING_RAN
    lines.append(f"Verdict: {verdict}")
    lines.append("=====================================")
    print("\n".join(lines))
    return code


def cmd_mcp(args) -> int:
    from .mcp import main as mcp_main

    return mcp_main(url=args.url, token=args.client_token, socket_path=args.socket)


def _scheduler_status_line(status: dict) -> str:
    if not status.get("paused"):
        return "scheduler running"
    parts = ["scheduler paused"]
    if status.get("reason"):
        parts.append(f"({status['reason']})")
    if status.get("until"):
        from datetime import datetime, timezone

        parts.append("until " + datetime.fromtimestamp(status["until"] / 1_000_000, tz=timezone.utc).isoformat())
    if status.get("suppress_rules"):
        parts.append("rules suppressed")
    held = status.get("held") or 0
    if held:
        parts.append(f"{held} run{'s' if held != 1 else ''} held")
    return " ".join(parts)


def cmd_pause(args) -> int:
    from . import client as client_module

    server = client_module.find_server(engine.resolved_home())
    if server is None:
        print("error: pause needs a running server (start `cereyan serve`)", file=sys.stderr)
        return EXIT_SCHEDULING
    try:
        status = server.pause_scheduler(reason=args.reason, until=args.until, suppress_rules=args.suppress_rules)
    except ValueError as exc:
        print(f"error: --until: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    except client_module.ApiError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    print(json.dumps(status) if args.json else _scheduler_status_line(status))
    return EXIT_OK


def cmd_resume(args) -> int:
    from . import client as client_module

    server = client_module.find_server(engine.resolved_home())
    if server is None:
        print("error: resume needs a running server (start `cereyan serve`)", file=sys.stderr)
        return EXIT_SCHEDULING
    try:
        status = server.resume_scheduler()
    except client_module.ApiError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    print(json.dumps(status) if args.json else _scheduler_status_line(status))
    return EXIT_OK


def cmd_backfill(args) -> int:
    from . import client as client_module

    server = client_module.find_server(engine.resolved_home())
    if server is None:
        print("error: backfill needs a running server (start `cereyan serve`)", file=sys.stderr)
        return EXIT_SCHEDULING
    project = None
    name = args.flow
    if "/" in name:
        project, name = name.split("/", 1)
    matches = [f for f in server.flows(project) if f["name"] == name]
    if not matches:
        print(f"error: no flow named {args.flow!r} is registered", file=sys.stderr)
        return EXIT_SCHEDULING
    if len(matches) > 1:
        print(f"error: flow {name!r} exists in several projects; use project/flow", file=sys.stderr)
        return EXIT_SCHEDULING
    extra = parse_params(args.extra)
    try:
        status = server.backfill(
            matches[0]["id"], args.param, args.start, args.end, interval=args.interval,
            concurrency=args.concurrency, extra_parameters=extra, reverse=args.reverse,
        )
    except CereyanError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    if args.json:
        print(json.dumps(status, indent=2))
    else:
        print(f"backfill {status['id']} created {status['total']} run(s) tagged {status['tag']}")
    return EXIT_OK


# ---------------------------------------------------------------------------
# runs ls


def _fmt_time(micros: int | None) -> str:
    if micros is None:
        return "-"
    return datetime.fromtimestamp(micros / 1_000_000, tz=timezone.utc).strftime("%Y-%m-%d %H:%M:%S")


def _fmt_micros(value) -> str:
    if value is None:
        return "-"
    seconds = value / 1_000_000
    return f"{seconds:.1f}s" if abs(seconds) < 90 else f"{seconds / 60:.1f}m"


def cmd_runs_compare(args) -> int:
    from . import client as client_module

    server = client_module.find_server(engine.resolved_home())
    if server is None:
        print("error: runs compare needs a running server (start `cereyan serve`)", file=sys.stderr)
        return EXIT_SCHEDULING
    try:
        c = server.compare_runs(args.baseline, args.other)
    except client_module.ApiError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING
    if args.json:
        print(json.dumps(c, indent=2))
        return EXIT_OK
    left, right, s = c["left"], c["right"], c["summary"]
    print(f"{left['name']} ({left['state']['type']}, {_fmt_micros(left['total_run_time'])}) -> "
          f"{right['name']} ({right['state']['type']}, {_fmt_micros(right['total_run_time'])})"
          + ("" if c["same_flow"] else "  [different flows]"))
    print(f"{s['parameters_changed']} parameter(s) changed, {s['tasks_state_changed']} task state(s) changed, "
          f"{s['tasks_duration_changed']} task duration(s) moved, {s['new_errors']} new error(s), "
          f"{s['artifacts_changed']} artifact difference(s)")
    for p in c["parameters"]:
        if p["changed"]:
            print(f"  param {p['key']}: {json.dumps(p['left'])} -> {json.dumps(p['right'])}")
    for t in c["tasks"]:
        l, r = t["left"], t["right"]
        mark = " <- first divergence" if t["key"] == c["first_divergence"] else ""
        print(f"  task {t['key']}: {l['state']['type'] if l else '-'} {_fmt_micros(l['duration']) if l else ''} -> "
              f"{r['state']['type'] if r else '-'} {_fmt_micros(r['duration']) if r else ''}{mark}")
    for m in c["new_errors"]:
        print(f"  new error: {m}")
    return EXIT_OK


def cmd_runs_ls(args) -> int:
    from . import client as client_module

    filt: dict[str, Any] = {"limit": max(1, min(args.limit, 500))}
    if args.flow:
        filt["flow"] = args.flow
    if args.project:
        filt["project"] = args.project
    if args.group:
        filt["group"] = args.group
    if args.state:
        filt["state_type"] = args.state
    server = client_module.find_server(engine.resolved_home())
    if server is not None:
        page = server.runs(**filt)
    else:
        store = engine.get_store()
        page = json.loads(store.list_runs(json.dumps(filt)))
    items = page["items"]
    if args.json:
        print(json.dumps(items, indent=2))
        return EXIT_OK
    if not items:
        print("no runs")
        return EXIT_OK
    rows = [
        (
            r["name"],
            f"{r['project']}/{r['flow_name']}",
            r["state"]["name"],
            _fmt_time(r.get("start_time") or r.get("created_at")),
            _fmt_duration(r.get("total_run_time")),
        )
        for r in items
    ]
    headers = ("NAME", "FLOW", "STATE", "START", "DURATION")
    widths = [max(len(h), *(len(row[i]) for row in rows)) for i, h in enumerate(headers)]
    print("  ".join(h.ljust(widths[i]) for i, h in enumerate(headers)))
    for row in rows:
        print("  ".join(cell.ljust(widths[i]) for i, cell in enumerate(row)))
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
