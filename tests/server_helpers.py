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
            os.kill(int(info.get("pid", 0)), 0)
            return True
        except OSError:
            return False

    def read_log(self) -> str:
        try:
            with open(self.log_path, "rb") as fh:
                return fh.read().decode("utf-8", "replace")
        except OSError:
            return ""

    def engine_pids(self) -> list[int]:
        try:
            return [e["pid"] for e in self.client.server()["engines"] if e.get("pid")]
        except Exception:
            return []

    def stop(self, kill_engines: bool = True, timeout: float = 40.0) -> int:
        pids = self.engine_pids() if kill_engines else []
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        for pid in pids:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
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
                os.kill(e["pid"], signal.SIGKILL)
    except Exception:
        pass
