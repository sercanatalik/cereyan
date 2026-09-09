"""The engine child process: ``python -m cereyan.engine``.

Imports one module, then pulls runs of its flows from the server and
executes them through the shared runner with a reporter backend. Survives
server restarts by retrying with backoff inside the Rust client.
"""

from __future__ import annotations

import argparse
import importlib
import json
import os
import signal
import sys
import threading
import time
import traceback

from .. import _core, apps
from ..client import read_discovery
from .backends import ReporterBackend
from .runner import RunInfo, execute_run, suppress_top_level_runs


def _find_flow(project: str, name: str):
    for app in apps.all_apps():
        if app.name == project and name in app.flows:
            return app.flows[name]
    for app in apps.all_apps():
        if name in app.flows:
            return app.flows[name]
    return None


class _CancelWatcher(threading.Thread):
    """Interrupts the main thread when the server asks for a cancel."""

    def __init__(self, client: _core.Client, run_id: int) -> None:
        super().__init__(daemon=True, name="cereyan-cancel-watch")
        self.client = client
        self.run_id = run_id
        self._stop = threading.Event()
        self.fired = False

    def run(self) -> None:
        main = threading.main_thread().ident
        while not self._stop.wait(0.2):
            if self.client.cancel_requested(self.run_id) and not self.fired:
                self.fired = True
                # A real signal to the main thread interrupts blocking calls
                # such as time.sleep; the default handler raises KeyboardInterrupt.
                try:
                    signal.pthread_kill(main, signal.SIGINT)
                except (AttributeError, OSError):
                    import _thread

                    _thread.interrupt_main()
                return

    def stop(self) -> None:
        self._stop.set()


def execute_job(client: _core.Client, work: dict, module) -> None:
    """Non-run jobs: crash hooks and backfill prefilters."""
    kind = work.get("kind")
    flow = _find_flow(work["project"], work["flow"])
    payload = work.get("payload") or {}
    if kind == "hooks":
        if flow is None:
            return
        run_id = int(work["run_id"])
        run = {"id": run_id, "external_id": work.get("external_id"), "name": work.get("run_name"), "flow": flow.name,
               "project": flow.project, "parameters": work.get("parameters") or {}}
        state = {"type": payload.get("state", "Crashed"), "name": payload.get("state", "Crashed")}
        from .runner import run_hooks
        import logging

        hooks = flow.on_crashed if state["type"] == "Crashed" else []
        run_hooks(hooks, flow, run, state, logging.getLogger("cereyan.run"))
        return
    if kind == "bulk_complete":
        backfill_id = payload.get("backfill_id")
        skip: list[str] = []
        try:
            status, text = client.get(f"/api/backfills/{backfill_id}")
            info = json.loads(text) if status < 300 else {}
            parameter = info.get("parameter")
            if flow is not None and flow.bulk_complete is not None and parameter:
                status, text = client.get(f"/api/runs?backfill_id={backfill_id}&limit=500&sort=created_asc")
                values = []
                cursor = None
                while True:
                    page = json.loads(text) if status < 300 else {"items": []}
                    values.extend(str(r["parameters"].get(parameter)) for r in page["items"])
                    cursor = page.get("next_cursor")
                    if not cursor:
                        break
                    status, text = client.get(f"/api/runs?backfill_id={backfill_id}&limit=500&sort=created_asc&cursor={cursor}")
                done = flow.bulk_complete(values)
                skip = [str(v) for v in (done or [])]
        except Exception:  # noqa: BLE001
            print(f"cereyan engine: bulk_complete failed:\n{traceback.format_exc()}", file=sys.stderr)
        client.post(f"/api/backfills/{backfill_id}/prefilter", json.dumps({"skip": skip}))
        return


def execute_work(client: _core.Client, work: dict, module) -> None:
    if work.get("kind", "run") != "run":
        execute_job(client, work, module)
        return
    run_id = int(work["run_id"])
    backend = ReporterBackend(client, run_id)
    flow = _find_flow(work["project"], work["flow"])
    try:
        if flow is None:
            backend.transition_run("Pending")
            backend.transition_run("Running")
            backend.transition_run(
                "Failed", None, f"flow {work['project']}/{work['flow']} is not defined in module {module.__name__}", None
            )
            return
        try:
            values = flow.coerce(dict(work.get("parameters") or {}))
        except Exception as exc:  # parameter errors surface as a failed run
            backend.transition_run("Pending")
            backend.transition_run("Running")
            backend.transition_run("Failed", None, f"{type(exc).__name__}: {exc}", {"traceback": traceback.format_exc()})
            return
        watcher = _CancelWatcher(client, run_id)
        watcher.start()
        try:
            execute_run(flow, values, backend, RunInfo(run_id, work["external_id"], work["run_name"]))
        except KeyboardInterrupt:
            if not watcher.fired:
                raise
        except Exception:
            pass  # recorded as Failed by execute_run
        finally:
            watcher.stop()
    finally:
        backend.close()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m cereyan.engine")
    parser.add_argument("--server", default=os.environ.get("CEREYAN_SERVER"))
    parser.add_argument("--engine-id", default=os.environ.get("CEREYAN_ENGINE_ID") or f"engine-{os.getpid()}")
    parser.add_argument("--source-dir", required=True)
    parser.add_argument("--module", required=True)
    parser.add_argument("--once", action="store_true", help="exit after one run (isolated flows)")
    parser.add_argument("--nice", type=int, default=0, help="lower the OS priority by this much (Unix)")
    args = parser.parse_args(argv)

    server = args.server
    if not server:
        info = read_discovery(None)
        if not info:
            print("cereyan engine: no server URL and no server.json", file=sys.stderr)
            return 2
        server = info.get("url") or f"http://{info['host']}:{info['port']}"

    client = _core.Client(server, args.engine_id, os.environ.get("CEREYAN_TOKEN") or None)
    source_dir = os.path.abspath(args.source_dir)
    if source_dir not in sys.path:
        sys.path.insert(0, source_dir)
    isolated = args.once
    nice = 0
    if args.nice > 0 and hasattr(os, "nice"):
        try:
            os.nice(args.nice)
            nice = args.nice
        except OSError as exc:  # noqa: PERF203
            print(f"cereyan engine: could not lower priority: {exc}", file=sys.stderr)
    suppress_top_level_runs(True, "engine child")
    try:
        # Imported once, outside the work loop below, so module-level state — an
        # HTTP client and its connection pool, a warmed cache — is shared by every
        # run this engine serves. docs/guides/fetch-from-an-api.md tells readers to
        # rely on that, and tests/test_module_state_reuse.py pins it. Moving this
        # into the loop changes a documented guarantee, not just a detail.
        module = importlib.import_module(args.module)
    except BaseException:
        tb = traceback.format_exc()
        print(f"cereyan engine: failed to import {args.module}:\n{tb}", file=sys.stderr)
        try:
            client.report_failed(source_dir, args.module, isolated, tb, nice)
        finally:
            client.close()
        return 1

    pid = os.getpid()
    code = 0
    try:
        while True:
            try:
                response = json.loads(client.get_work(pid, source_dir, args.module, isolated, 30000, nice))
            except Exception as exc:
                print(f"cereyan engine: work request failed: {exc}", file=sys.stderr)
                time.sleep(1.0)
                continue
            if response.get("exit"):
                break
            work = response.get("run")
            if not work:
                continue
            execute_work(client, work, module)
            if isolated:
                break
    except KeyboardInterrupt:
        code = 130
    finally:
        client.close()
    return code
