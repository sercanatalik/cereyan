"""The ``cereyan`` command: ``run`` and ``runs ls`` in phase 0."""

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
    serve.set_defaults(func=cmd_serve)

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

    mcp = sub.add_parser("mcp", help="MCP server over stdio for agent hosts (proxies to the running server)")
    mcp.add_argument("--url", help="server URL (default: from server.json)")
    mcp.add_argument("--socket", help="Unix socket path of the server")
    mcp.set_defaults(func=cmd_mcp)

    runs = sub.add_parser("runs", help="inspect runs")
    runs_sub = runs.add_subparsers(dest="runs_command", required=True)
    ls = runs_sub.add_parser("ls", help="list recent runs")
    ls.add_argument("--flow", help="only runs of this flow name")
    ls.add_argument("--project", help="only runs of flows in this project")
    ls.add_argument("--state", help="state type, e.g. Failed")
    ls.add_argument("--limit", type=int, default=20, help="number of runs to show")
    ls.add_argument("--json", action="store_true", help="print the runs as JSON")
    ls.set_defaults(func=cmd_runs_ls)
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
        )
    except CereyanError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return EXIT_SCHEDULING


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


def cmd_runs_ls(args) -> int:
    from . import client as client_module

    filt: dict[str, Any] = {"limit": max(1, min(args.limit, 500))}
    if args.flow:
        filt["flow"] = args.flow
    if args.project:
        filt["project"] = args.project
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
