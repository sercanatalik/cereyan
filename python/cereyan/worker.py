"""``cereyan worker``: another machine's processors for a server's queue.

A worker imports its own checkout of the project, tells the server which flows
it can run and the fingerprint of each module, and then heartbeats. Each
heartbeat answer carries commands: start an engine for a module, stop one,
cancel a run, drain, resume. Engines it starts talk to the server directly, over
the same protocol as the server's own engines. The worker opens no port.
"""

from __future__ import annotations

import os
import platform
import shutil
import signal
import socket
import subprocess
import sys
import time
from typing import Any

from . import __version__, _core, apps, engine
from .client import ApiError, Client, ServerUnavailable
from .exceptions import CereyanError

#: Seconds to wait before registering again after the server could not be reached.
RETRY_SECS = 5.0


def _git(directory: str, *args: str) -> str | None:
    if shutil.which("git") is None:
        return None
    try:
        out = subprocess.run(["git", "-C", directory, *args], capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip() if out.returncode == 0 else None


def git_state(directory: str) -> str | None:
    """``branch · sha · clean`` (or ``dirty``) for a checkout, or ``None`` outside git."""
    sha = _git(directory, "rev-parse", "--short", "HEAD")
    if sha is None:
        return None
    branch = _git(directory, "rev-parse", "--abbrev-ref", "HEAD") or "?"
    dirty = bool(_git(directory, "status", "--porcelain"))
    return f"{branch} · {sha} · {'dirty' if dirty else 'clean'}"


def _memory() -> tuple[int | None, int | None]:
    """(total, available) bytes where the platform says; ``None`` otherwise."""
    total = available = None
    try:
        total = os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")
    except (ValueError, OSError, AttributeError):
        pass
    try:
        with open("/proc/meminfo") as fh:
            for line in fh:
                if line.startswith("MemAvailable:"):
                    available = int(line.split()[1]) * 1024
    except OSError:
        pass
    return total, available


def _gpus() -> list[str]:
    """GPU names from ``nvidia-smi`` when it is installed; nothing else is probed."""
    if shutil.which("nvidia-smi") is None:
        return []
    try:
        out = subprocess.run(["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"],
                             capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return []
    return [line.strip() for line in out.stdout.splitlines() if line.strip()]


def host_meta(directory: str, host: str) -> dict[str, Any]:
    """What the worker reports about its machine at registration."""
    total, available = _memory()
    return {
        "hostname": socket.gethostname(),
        "platform": platform.platform(),
        "arch": platform.machine(),
        "gpus": _gpus(),
        "memory_total": total,
        "memory_available": available,
        "python": platform.python_version(),
        "pid": os.getpid(),
        "started_at": int(time.time() * 1_000_000),
        "checkout": directory,
        "git": git_state(directory),
        "connection": "TLS" if host.lower().startswith("https://") else "plain HTTP",
        "auth": "server token",
    }


def _flows(directory: str) -> list[dict[str, str]]:
    """Every flow the checkout defines, with the fingerprint of its module."""
    out = []
    for app in apps.all_apps():
        for f in app.flows.values():
            digest = _core.module_fingerprint(f.source_dir, f.module)
            if digest is None:
                continue
            out.append({"project": f.project, "flow": f.name, "module": f.module, "module_hash": digest})
    out.sort(key=lambda d: (d["project"], d["flow"]))
    return out


def _fingerprints_now(flows: list[dict[str, str]]) -> list[dict[str, str]]:
    """The same flows with each module fingerprinted again: the checkout may have changed."""
    by_module: dict[str, str | None] = {}
    out = []
    for f in flows:
        module = f["module"]
        if module not in by_module:
            by_module[module] = _core.module_fingerprint(_source_dir(f), module)
        digest = by_module[module]
        if digest is not None:
            out.append({**f, "module_hash": digest})
    return out


_SOURCE_DIRS: dict[str, str] = {}


def _source_dir(flow: dict[str, str]) -> str:
    return _SOURCE_DIRS[flow["module"]]


class Worker:
    """One worker process. `run` blocks until the process is told to stop."""

    def __init__(self, host: str, directory: str, *, name: str | None = None, processors: int = 1,
                 labels: dict[str, str] | None = None, shared_paths: list[str] | None = None,
                 token: str | None = None) -> None:
        self.host = host.rstrip("/")
        self.directory = os.path.abspath(directory)
        self.name = name or socket.gethostname()
        self.cpus = os.cpu_count() or 1
        self.processors = max(1, min(int(processors), self.cpus))
        self.labels = dict(labels or {})
        self.shared_paths = [os.path.abspath(p) for p in (shared_paths or [])]
        self.token = token
        self.client = Client(self.host, token=token)
        self.worker_id: int | None = None
        self.interval = 5.0
        self.engines: dict[str, subprocess.Popen] = {}
        self.stopping = False
        self.flows: list[dict[str, str]] = []

    # -- setup ------------------------------------------------------------

    def load(self) -> None:
        """Import the checkout as `serve` does, without opening a store."""
        from .serve import discover_modules, import_modules

        if not os.path.isdir(self.directory):
            raise CereyanError(f"{self.directory} is not a directory")
        engine.runner.suppress_top_level_runs(True, "cereyan worker is importing modules")
        try:
            for name, tb in import_modules(self.directory, discover_modules(self.directory)):
                print(f"warning: could not import {name}:\n{tb}", file=sys.stderr)
        finally:
            engine.runner.suppress_top_level_runs(False)
        for app in apps.all_apps():
            for f in app.flows.values():
                _SOURCE_DIRS[f.module] = f.source_dir
        self.flows = _flows(self.directory)
        if not self.flows:
            print(f"warning: no flows found under {self.directory}", file=sys.stderr)

    def register(self) -> None:
        body = {
            "name": self.name,
            "version": __version__,
            "cpus": self.cpus,
            "processors": self.processors,
            "labels": self.labels,
            "shared_paths": self.shared_paths,
            "meta": host_meta(self.directory, self.host),
            "flows": self.flows,
        }
        answer = self.client._request("POST", "/api/workers/register", body=body)
        self.worker_id = int(answer["worker_id"])
        self.interval = float(answer.get("heartbeat_secs") or 5)
        state = answer.get("state", "online")
        print(f"cereyan worker: registered as {self.name} (id {self.worker_id}, {state}) with {self.host}; "
              f"{self.processors} processor(s), {len(self.flows)} flow(s)", file=sys.stderr)
        for flow in answer.get("refused") or []:
            print(f"cereyan worker: the server does not know {flow}; it will not run here", file=sys.stderr)
        for module in answer.get("drift") or []:
            print(f"cereyan worker: {module} differs from the server's code; update this checkout to run it",
                  file=sys.stderr)

    # -- engines ----------------------------------------------------------

    def _env(self) -> dict[str, str]:
        env = dict(os.environ)
        if self.token:
            env["CEREYAN_TOKEN"] = self.token
        env["CEREYAN_WORKER"] = str(self.worker_id)
        env["CEREYAN_SHARED_PATHS"] = os.pathsep.join(self.shared_paths)
        env["PYTHONUNBUFFERED"] = "1"
        return env

    def spawn(self, cmd: dict[str, Any]) -> None:
        engine_id = str(cmd["engine_id"])
        if engine_id in self.engines or self.stopping:
            return
        argv = [sys.executable, "-m", "cereyan.engine", "--server", self.host, "--engine-id", engine_id,
                "--source-dir", self.directory, "--module", str(cmd["module"])]
        if cmd.get("isolated"):
            argv.append("--once")
        if int(cmd.get("nice") or 0) > 0:
            argv += ["--nice", str(int(cmd["nice"]))]
        # The checkout is the engine's working directory, so a relative path in
        # a flow means the same file it means on the server.
        self.engines[engine_id] = subprocess.Popen(argv, cwd=self.directory, env=self._env(), stdin=subprocess.DEVNULL)

    def stop_engine(self, engine_id: str | None, grace: float = 10.0) -> None:
        proc = self.engines.get(engine_id or "")
        if proc is None or proc.poll() is not None:
            return
        proc.terminate()
        try:
            proc.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            proc.kill()

    def reap(self) -> None:
        for engine_id, proc in list(self.engines.items()):
            if proc.poll() is not None:
                del self.engines[engine_id]

    # -- loop -------------------------------------------------------------

    def heartbeat(self) -> None:
        self.reap()
        now = _fingerprints_now(self.flows)
        changed = now != self.flows
        self.flows = now
        _, available = _memory()
        body: dict[str, Any] = {
            "engines": sorted(self.engines),
            "meta": {"memory_available": available, "engines": len(self.engines), "git": git_state(self.directory)},
        }
        if changed:
            body["flows"] = now
        try:
            answer = self.client._request("POST", f"/api/workers/{self.worker_id}/heartbeat", body=body)
        except ApiError as exc:
            if exc.status == 409 and isinstance(exc.body, dict) and exc.body.get("register"):
                self.register()
                return
            raise
        for cmd in answer.get("commands") or []:
            self.handle(cmd)

    def handle(self, cmd: dict[str, Any]) -> None:
        kind = cmd.get("cmd")
        if kind == "spawn":
            self.spawn(cmd)
        elif kind in ("exit", "cancel"):
            self.stop_engine(cmd.get("engine_id"))
        elif kind == "drain":
            print("cereyan worker: draining; current runs finish, no new ones start here", file=sys.stderr)
        elif kind == "resume":
            print("cereyan worker: resumed", file=sys.stderr)

    def drain_and_wait(self) -> None:
        """Tell the server to send nothing new, then wait for running engines to end."""
        try:
            self.client._request("POST", f"/api/workers/{self.worker_id}/drain")
        except (ApiError, ServerUnavailable):
            pass
        while self.engines:
            self.reap()
            time.sleep(0.5)

    def run(self) -> int:
        self.load()
        while self.worker_id is None:
            try:
                self.register()
            except ServerUnavailable as exc:
                print(f"cereyan worker: {exc}; retrying in {RETRY_SECS:g} s", file=sys.stderr)
                time.sleep(RETRY_SECS)
            except ApiError as exc:
                message = exc.body.get("error") if isinstance(exc.body, dict) else exc.body
                print(f"cereyan worker: the server refused registration ({exc.status}): {message}", file=sys.stderr)
                return 2

        def on_signal(signum, frame):  # noqa: ARG001 - signal handler signature
            if self.stopping:
                # A second signal: leave the running engines to finish on their own.
                raise SystemExit(0)
            self.stopping = True

        signal.signal(signal.SIGTERM, on_signal)
        signal.signal(signal.SIGINT, on_signal)
        while not self.stopping:
            try:
                self.heartbeat()
            except (ApiError, ServerUnavailable) as exc:
                print(f"cereyan worker: heartbeat failed: {exc}", file=sys.stderr)
            deadline = time.time() + self.interval
            while time.time() < deadline and not self.stopping:
                time.sleep(0.2)
        print("cereyan worker: stopping; waiting for running engines to finish (signal again to leave them)",
              file=sys.stderr)
        self.drain_and_wait()
        return 0


def parse_labels(text: str | None) -> dict[str, str]:
    """``gpu=true,zone=eu`` as a dict."""
    out: dict[str, str] = {}
    for part in (text or "").split(","):
        part = part.strip()
        if not part:
            continue
        key, _, value = part.partition("=")
        out[key.strip()] = value.strip()
    return out


def run_worker(host: str | None, directory: str | None = None, *, name: str | None = None,
               processors: int | None = None, labels: str | dict | None = None,
               shared_paths: list[str] | None = None, token: str | None = None,
               token_file: str | None = None) -> int:
    """Start a worker from the CLI flags, then ``[worker]`` in the checkout's ``cereyan.toml``."""
    from .config import worker_settings

    directory = os.path.abspath(directory or os.getcwd())
    settings = worker_settings(directory)
    host = host or settings.get("host")
    if not host:
        raise CereyanError("a worker needs --host (or [worker] host in cereyan.toml): the server's URL")
    token_file = token_file or settings.get("token_file")
    if token is None and token_file:
        with open(os.path.expanduser(token_file)) as fh:
            token = fh.read().strip()
    token = token or os.environ.get("CEREYAN_TOKEN")
    if not token:
        raise CereyanError("a worker needs the server's token: pass --token, set CEREYAN_TOKEN, or use --token-file")
    label_map = labels if isinstance(labels, dict) else parse_labels(labels)
    if not label_map and isinstance(settings.get("labels"), dict):
        label_map = {str(k): str(v) for k, v in settings["labels"].items()}
    worker = Worker(
        host,
        directory,
        name=name or settings.get("name"),
        processors=processors or int(settings.get("processors") or 1),
        labels=label_map,
        shared_paths=shared_paths or list(settings.get("shared_paths") or []),
        token=token,
    )
    return worker.run()
