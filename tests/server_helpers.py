"""Helpers for tests that start a real ``cereyan serve`` subprocess."""

from __future__ import annotations

import json
import os
import signal
import socket
import subprocess
import sys
import time
import urllib.request

from cereyan.client import Client

WINDOWS = sys.platform == "win32"

# Process control, in one place, because the platforms differ in ways that bite.
#
# On Windows `os.kill` understands only CTRL_C_EVENT and CTRL_BREAK_EVENT; every
# other value, zero included, terminates the target through TerminateProcess. So
# the usual `os.kill(pid, 0)` liveness probe does not ask whether a process is
# alive there, it makes sure it is not. Liveness goes through OpenProcess instead.
#
# `Popen.send_signal(SIGTERM)` is the matching trap: on Windows it is a hard kill,
# so a server stopped that way never runs its shutdown path, and a test asserting
# graceful shutdown fails on the harness rather than on the code it is testing.

if WINDOWS:  # pragma: no cover - exercised on Windows CI
    import ctypes
    from ctypes import wintypes

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    _STILL_ACTIVE = 259

    def is_alive(pid: int) -> bool:
        """True while the process exists, without touching it."""
        handle = _kernel32.OpenProcess(_PROCESS_QUERY_LIMITED_INFORMATION, False, int(pid))
        if not handle:
            return False
        try:
            code = wintypes.DWORD()
            if not _kernel32.GetExitCodeProcess(handle, ctypes.byref(code)):
                return False
            return code.value == _STILL_ACTIVE
        finally:
            _kernel32.CloseHandle(handle)

    def terminate(pid: int) -> None:
        """Ask the process to stop; Windows has no SIGTERM, so this is a hard stop."""
        kill(pid)

    def kill(pid: int) -> None:
        if not is_alive(pid):
            return
        subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True, check=False)

else:

    def is_alive(pid: int) -> bool:
        """True while the process exists, without touching it."""
        try:
            os.kill(int(pid), 0)
        except OSError:
            return False
        return True

    def terminate(pid: int) -> None:
        try:
            os.kill(int(pid), signal.SIGTERM)
        except OSError:
            pass

    def kill(pid: int) -> None:
        try:
            os.kill(int(pid), signal.SIGKILL)
        except OSError:
            pass


def stop_server(proc: subprocess.Popen, timeout: float = 40.0) -> int:
    """Ask a serve subprocess to shut down cooperatively, then force it.

    The cooperative step matters: the server removes its discovery file, flushes
    the store and ends idle engines on the way out, and tests assert all three.
    On Windows only CTRL_BREAK_EVENT reaches a child cooperatively, and only when
    it was created in its own process group.
    """
    if proc.poll() is not None:
        return proc.returncode
    try:
        if WINDOWS:  # pragma: no cover - exercised on Windows CI
            proc.send_signal(signal.CTRL_BREAK_EVENT)
        else:
            proc.send_signal(signal.SIGTERM)
    except (OSError, ValueError):
        proc.kill()
    try:
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    return proc.returncode

PIPELINE = '''
import logging, os, signal, sys, time
from datetime import date
from cereyan import App, flow, task, get_run_logger, HTTPError, Request
import cereyan

app = App("proj")

@task
def step(n: int = 1) -> int:
    logging.getLogger(__name__).warning("slow")
    return n

@task(log_prints=True)
def shout():
    print("hello from task")

@app.flow
def etl(day: date, n: int = 1) -> str:
    get_run_logger().info("day %s", day)
    step(n)
    return str(day)

@app.flow(log_prints=True)
def printer():
    print("hello")
    shout()

@app.flow
def fail():
    step()
    raise ValueError("bad")

@app.flow
def sleepy(seconds: float = 30.0):
    get_run_logger().info("sleeping")
    time.sleep(seconds)

@app.flow
def stubborn(seconds: float = 60.0):
    # Ignores cooperative cancellation; SIGTERM still ends the process.
    end = time.time() + seconds
    while time.time() < end:
        try:
            time.sleep(0.1)
        except KeyboardInterrupt:
            pass

@app.flow
def immortal(seconds: float = 60.0):
    # Ignores cooperative cancellation and SIGTERM; only SIGKILL works.
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    end = time.time() + seconds
    while time.time() < end:
        try:
            time.sleep(0.1)
        except KeyboardInterrupt:
            pass

@app.flow(isolated=True)
def isolated_flow():
    return os.getpid()

@app.flow
def pid_flow():
    return os.getpid()

@app.get("/api/ext/ping")
def ping():
    return {"ok": True}

@app.get("/api/ext/orders/{id}")
def order(id: int, verbose: bool = False):
    return {"id": id, "verbose": verbose}

@app.post("/api/ext/items")
def create_item(request: Request):
    return ({"received": request.json()}, 201)

@app.get("/api/ext/teapot")
def teapot():
    raise HTTPError(418, "short and stout")

@app.get("/api/ext/boom")
def boom():
    raise RuntimeError("kaboom")

@app.get("/health")
def health():
    return "fine"

@app.post("/api/ext/webhook")
def webhook(day: str = "2026-09-06"):
    run = cereyan.client.run("etl", project="proj", day=day)
    return {"run_id": run["id"]}
'''


PIDS_MODULE = """
import os
from cereyan import flow, task, ProcessRunner

@task
def pid(i):
    import time
    time.sleep(0.3)
    return os.getpid()

@flow(runner=ProcessRunner(4))
def parallel_flow():
    return [f.result() for f in pid.map([1, 2, 3, 4])]
"""


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class ServerProcess:
    def __init__(self, home: str, directory: str, *, port: int = 0, env: dict | None = None, extra: list[str] | None = None):
        self.home = home
        self.directory = directory
        full_env = dict(os.environ)
        full_env["CEREYAN_HOME"] = str(home)
        full_env["CEREYAN_NO_BROWSER"] = "1"
        full_env.pop("CEREYAN_PORT", None)
        full_env.pop("CEREYAN_HOST", None)
        if env:
            full_env.update(env)
        os.makedirs(str(home), exist_ok=True)
        self.log_path = os.path.join(str(home), "serve.log")
        self.log = open(self.log_path, "ab")
        self.proc = subprocess.Popen(
            [sys.executable, "-m", "cereyan", "serve", str(directory), "--port", str(port), "--no-open", *(extra or [])],
            env=full_env,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            # Windows: its own group, so CTRL_BREAK_EVENT reaches the server and
            # stops there instead of travelling back up and interrupting pytest.
            **({"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if WINDOWS else {}),
        )
        self.info = self._wait_ready()
        self.client = Client(self.info["url"])

    def _wait_ready(self, timeout: float = 30.0) -> dict:
        path = os.path.join(str(self.home), "server.json")
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"serve exited early with {self.proc.returncode}:\n{self.read_log()}")
            try:
                with open(path) as fh:
                    info = json.load(fh)
                if info.get("pid") == self.proc.pid or self._alive(info):
                    with urllib.request.urlopen(info["url"] + "/api/health", timeout=2) as r:
                        if r.status == 200:
                            return info
            except (OSError, ValueError):
                pass
            time.sleep(0.05)
        raise RuntimeError(f"server did not start:\n{self.read_log()}")

    @staticmethod
    def _alive(info: dict) -> bool:
        try:
            return is_alive(int(info.get("pid", 0)))
        except (OSError, TypeError, ValueError):
            return False

    def read_log(self) -> str:
        try:
            with open(self.log_path, "rb") as fh:
                return fh.read().decode("utf-8", "replace")
        except OSError:
            return ""

    def wait_idle(self, timeout: float = 30.0, settle: float = 2.0) -> None:
        """Wait until no engine holds a run and the tail of its report has landed.

        `wait_run` returns when the *server* records the terminal state. The engine is
        still flushing its last events and log lines then, and that flush retries for
        MAX_RETRY (600 s in crates/py/src/client.rs) so a restarted server does not lose
        the outcome. A test that kills the server, or rewrites the rows the engine is
        about to add to, has to let it finish first.
        """
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                engines = self.client.server()["engines"]
            except Exception:  # noqa: BLE001 - the server may be mid-restart
                engines = []
            if engines and not any(e.get("current_run") for e in engines):
                break
            time.sleep(0.1)
        time.sleep(settle)

    def engine_pids(self) -> list[int]:
        try:
            return [e["pid"] for e in self.client.server()["engines"] if e.get("pid")]
        except Exception:
            return []

    def stop(self, kill_engines: bool = True, timeout: float = 40.0) -> int:
        pids = self.engine_pids() if kill_engines else []
        stop_server(self.proc, timeout=timeout)
        for pid in pids:
            kill(pid)
        self.log.close()
        return self.proc.returncode

    def wait_run(self, run_id: int, timeout: float = 30.0, until=None) -> dict:
        deadline = time.time() + timeout
        while time.time() < deadline:
            run = self.client.get_run(run_id)
            if until is not None:
                if until(run):
                    return run
            elif run["state"]["type"] in ("Completed", "Failed", "Cancelled", "Crashed"):
                return run
            time.sleep(0.05)
        raise AssertionError(f"run {run_id} did not reach the expected state in time: {run['state']}\n{self.read_log()}")


def kill_engines_of(client: Client) -> None:
    try:
        for e in client.server()["engines"]:
            if e.get("pid"):
                kill(e["pid"])
    except Exception:
        pass
