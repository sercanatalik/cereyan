"""End-to-end performance benchmark against the targets in the
performance-targets spec. Generates its own fixtures. Usage:

    uv run python benches/e2e.py [--update-baseline] [--quick]

Prints a table against benches/baseline.<platform>.json and exits 1 when a target
regresses more than 20 percent from the baseline (or misses its ceiling).
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid

import platform

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
# Baselines are per platform: numbers from one machine do not transfer.
PLATFORM = f"{sys.platform}-{platform.machine()}".lower()
BASELINE = os.path.join(HERE, f"baseline.{PLATFORM}.json")
sys.path.insert(0, os.path.join(ROOT, "tests"))

TARGETS_MS = {
    "server_start_1m_runs": 200.0,
    "task_transitions_per_s": None,   # higher is better; ceiling below is a floor
    "log_lines_per_s": None,
    "runs_list_query_1m_ms": 10.0,
    "schedule_drift_ms": 50.0,
    "backfill_10k_create_ms": 1000.0,
    "warm_pool_overhead_ms": 5.0,
    "counts_ms": 5.0,
    # Microseconds, not milliseconds: this dict is ceilings for lower-is-better
    # numbers, and already mixes units via the _per_s keys above.
    "task_run_cost_us": 200.0,
}
FLOORS = {"task_transitions_per_s": 20_000.0, "log_lines_per_s": 100_000.0}

PIPELINE = '''
import logging, os, time
from datetime import date
from cereyan import App, flow, task, Interval
app = App("bench")

@task
def noop(i: int = 0):
    return i

@app.flow
def warm():
    return os.getpid()

@app.flow
def many_tasks(n: int = 2000):
    for i in range(n):
        noop(i)

@app.flow
def many_logs(n: int = 100000):
    from cereyan import get_run_logger
    log = get_run_logger()
    for i in range(n):
        log.info("line %d", i)

@app.flow
def daily(day: date = date(2026, 1, 1)):
    return str(day)
'''


def api(url, path, body=None, method=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url + path, data=data, method=method or ("POST" if body is not None else "GET"), headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as exc:
        raise RuntimeError(f"{method or 'POST' if body is not None else 'GET'} {path} -> {exc.code}: {exc.read().decode(errors='replace')}") from None


def wait_run(url, run_id, timeout=600):
    deadline = time.time() + timeout
    while time.time() < deadline:
        r = api(url, f"/api/runs/{run_id}")
        if r["state"]["type"] in ("Completed", "Failed", "Crashed", "Cancelled"):
            return r
        time.sleep(0.02)
    raise RuntimeError("run did not finish")


def seed_runs(home, n):
    """Insert n historical runs directly (the store's schema is stable)."""
    env = dict(os.environ, CEREYAN_HOME=home)
    subprocess.run([sys.executable, "-c", "import cereyan, cereyan.engine as e; s=e.get_store(); s.upsert_flow('bench','daily','pipeline',%r); s.flush()" % os.path.join(home, "proj")], env=env, check=True)
    db = sqlite3.connect(os.path.join(home, "db.sqlite"))
    flow_id = db.execute("SELECT id FROM flow").fetchone()[0]
    now = int(time.time() * 1_000_000)
    rows = ((uuid.uuid4().bytes, flow_id, f"h{i}", "{}", "[]", "Completed", "Completed", now - i, now - i, now - i + 5, 5) for i in range(n))
    db.executemany(
        "INSERT INTO run (external_id, flow_id, name, parameters, tags, state_type, state_name, created_at, start_time, end_time, total_run_time) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        rows,
    )
    db.commit()
    db.close()


class Server:
    def __init__(self, home, directory, env=None, port=0, open_browser=False):
        self.home = home
        full = dict(os.environ, CEREYAN_HOME=home, **(env or {}))
        if not open_browser:
            full["CEREYAN_NO_BROWSER"] = "1"
        args = [sys.executable, "-m", "cereyan", "serve", directory, "--port", str(port)] + ([] if open_browser else ["--no-open"])
        self.t0 = time.perf_counter()
        self.proc = subprocess.Popen(args, env=full, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        path = os.path.join(home, "server.json")
        while not os.path.exists(path):
            if self.proc.poll() is not None:
                raise RuntimeError("server exited")
            time.sleep(0.002)
        self.started_ms = (time.perf_counter() - self.t0) * 1000
        self.url = json.load(open(path))["url"]
        while True:
            try:
                api(self.url, "/api/health")
                break
            except Exception:
                time.sleep(0.01)

    def stop(self):
        try:
            for e in api(self.url, "/api/server")["engines"]:
                os.kill(e["pid"], 9)
        except Exception:
            pass
        self.proc.terminate()
        self.proc.wait(timeout=60)


def bench(quick: bool) -> dict:
    results = {}
    home = tempfile.mkdtemp(prefix="cereyan-bench-")
    proj = os.path.join(home, "proj")
    os.makedirs(proj)
    with open(os.path.join(proj, "pipeline.py"), "w") as fh:
        fh.write(PIPELINE)
    n_hist = 100_000 if quick else 1_000_000
    t0 = time.perf_counter()
    seed_runs(home, n_hist)
    print(f"seeded {n_hist} runs in {time.perf_counter() - t0:.1f}s", file=sys.stderr)
    srv = Server(home, proj)
    try:
        url = srv.url
        # Server start: measured by the Rust server (reconcile + bind) is inside the Python startup;
        # subtract interpreter startup by measuring a bare import.
        t0 = time.perf_counter()
        subprocess.run([sys.executable, "-c", "import cereyan"], check=True)
        interp_ms = (time.perf_counter() - t0) * 1000
        results["server_start_1m_runs"] = max(0.0, srv.started_ms - interp_ms)
        flows = {f["name"]: f for f in api(url, "/api/flows")}
        # Warm pool overhead: second run of a flow, Scheduled -> Running.
        first = api(url, f"/api/flows/{flows['warm']['id']}/runs", {"parameters": {}})
        wait_run(url, first["id"])
        overheads = []
        for _ in range(5):
            r = api(url, f"/api/flows/{flows['warm']['id']}/runs", {"parameters": {}})
            done = wait_run(url, r["id"])
            overheads.append((done["start_time"] - done["created_at"]) / 1000)
        results["warm_pool_overhead_ms"] = sorted(overheads)[len(overheads) // 2]
        # Task transitions: 2000 tasks = 6000 transitions plus creates.
        n_tasks = 2000
        r = api(url, f"/api/flows/{flows['many_tasks']['id']}/runs", {"parameters": {"n": n_tasks}})
        done = wait_run(url, r["id"])
        secs = (done["end_time"] - done["start_time"]) / 1e6
        results["task_transitions_per_s"] = (n_tasks * 3) / secs
        # The same run in the unit the workload is counted in: orchestration
        # cost for one short task run, Python task body included.
        results["task_run_cost_us"] = (done["end_time"] - done["start_time"]) / n_tasks
        # Logs.
        n_logs = 100_000
        r = api(url, f"/api/flows/{flows['many_logs']['id']}/runs", {"parameters": {"n": n_logs}})
        done = wait_run(url, r["id"])
        # The engine flushes every report before finalizing, so all lines are stored.
        stored = api(url, f"/api/runs/{r['id']}/logs?limit=1&after={0}")
        assert stored["items"], "no logs stored"
        secs = (done["end_time"] - done["start_time"]) / 1e6
        results["log_lines_per_s"] = n_logs / secs
        # Runs list at N runs.
        api(url, f"/api/runs?flow=daily&limit=50")
        t0 = time.perf_counter()
        for _ in range(20):
            api(url, f"/api/runs?flow=daily&limit=50")
        results["runs_list_query_1m_ms"] = (time.perf_counter() - t0) * 1000 / 20
        # Counts.
        t0 = time.perf_counter()
        for _ in range(50):
            api(url, "/api/counts")
        results["counts_ms"] = (time.perf_counter() - t0) * 1000 / 50
        # Backfill of 10k runs.
        t0 = time.perf_counter()
        status = api(url, f"/api/flows/{flows['daily']['id']}/backfill", {"parameter": "day", "start": "1990-01-01", "end": "2017-05-18", "interval": "1d", "concurrency": 1})
        results["backfill_10k_create_ms"] = (time.perf_counter() - t0) * 1000
        assert status["total"] == 10_000, status["total"]
        api(url, f"/api/backfills/{status['id']}/cancel", {})
        # Schedule drift: an interval schedule anchored 3 s ahead.
        anchor = int(time.time() * 1_000_000) + 3_000_000
        sched = api(url, f"/api/flows/{flows['warm']['id']}/schedules", {"kind": "interval", "interval": 600, "anchor": anchor - 600 * 1_000_000, "timezone": "UTC"})
        upcoming = api(url, f"/api/flows/{flows['warm']['id']}/upcoming")
        target = min(u["scheduled_time"] for u in upcoming)
        run_id = next(u["id"] for u in upcoming if u["scheduled_time"] == target)
        done = wait_run(url, run_id)
        results["schedule_drift_ms"] = (done["start_time"] - target) / 1000
        api(url, f"/api/schedules/{sched['id']}", None, "DELETE")
    finally:
        srv.stop()
    return results


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--update-baseline", action="store_true")
    ap.add_argument("--quick", action="store_true", help="seed 100k runs instead of 1M")
    ap.add_argument(
        "--no-ceilings",
        action="store_true",
        help="report absolute target misses without failing; regressions against a "
        "baseline for this platform still fail. For shared CI runners, whose speed the "
        "ceilings were never calibrated for",
    )
    args = ap.parse_args()
    results = bench(args.quick)
    baseline = json.load(open(BASELINE)) if os.path.exists(BASELINE) else {}
    if not baseline and not args.update_baseline:
        print(f"no baseline for {PLATFORM} ({BASELINE}); checking targets only", file=sys.stderr)
    failed = []
    print(f"{'target':28} {'result':>14} {'baseline':>14} {'ceiling':>10}  status")
    for name, value in results.items():
        base = baseline.get(name)
        higher_better = name in FLOORS
        ceiling = FLOORS.get(name) or TARGETS_MS.get(name)
        status = "ok"
        if ceiling is not None and ((higher_better and value < ceiling) or (not higher_better and value > ceiling)):
            status = "misses target" if args.no_ceilings else "MISSES TARGET"
        if base is not None:
            regressed = (value < base * 0.8) if higher_better else (value > base * 1.2)
            if regressed:
                status = "REGRESSED >20%"
        if status not in ("ok", "misses target"):
            failed.append(name)
        unit = "/s" if higher_better else (" µs" if name.endswith("_us") else " ms")
        print(f"{name:28} {value:14.2f}{unit:>0} {base if base is not None else float('nan'):14.2f} {ceiling if ceiling is not None else float('nan'):10.1f}  {status}")
    if args.update_baseline:
        json.dump(results, open(BASELINE, "w"), indent=2)
        print(f"baseline written to {BASELINE}")
        return 0
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
