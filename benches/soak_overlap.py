"""Overlap soak: fifteen scheduled flows whose runs outlast their interval,
run for an hour against a real server, then checked against the invariants
in the overlap-soak spec. Usage:

    uv run python benches/soak_overlap.py [--minutes 60] [--seed 1] [--quick] [--keep] [--url URL]

--quick divides intervals and durations by five and runs for twelve minutes.
--keep leaves the server on port 4200 with the browser open after the checks.
--url checks an already running server whose flows match the manifest.
Exits 1 when any invariant fails; writes benches/soak-report.json either way.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, replace
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from e2e import Server, api  # noqa: E402

REPORT = os.path.join(HERE, "soak-report.json")
MESSAGE = "previous run still active"
TERMINAL = {"Completed", "Failed", "Crashed", "Cancelled"}
SAMPLE_SECS = 5.0
GAP_WARN_SECS = 30.0
PROJECT = "soak"
QUICK_SCALE = 5
QUICK_MINUTES = 12


@dataclass(frozen=True)
class FlowSpec:
    """One soak flow. ``durations`` (seeded, cycled by tick index) overrides ``duration`` when set."""

    name: str
    policy: str
    interval: float  # seconds
    duration: float  # seconds
    cap: int
    control: bool = False
    durations: tuple[float, ...] = ()
    offset: float | None = None  # seconds after the window start of the first fire

    def duration_at(self, tick: int) -> float:
        if self.durations:
            return self.durations[tick % len(self.durations)]
        return self.duration

    def scaled(self, factor: int) -> FlowSpec:
        return replace(
            self,
            interval=self.interval / factor,
            duration=self.duration / factor,
            durations=tuple(d / factor for d in self.durations),
        )


# The fixed shape: three policies, five shapes each. Twelve flows outlast their interval;
# the four controls never reach their cap. Only anchors and the jitter list come from the seed.
MANIFEST: tuple[FlowSpec, ...] = (
    FlowSpec("enqueue_1m_90s", "enqueue", 60, 90, 1),
    FlowSpec("enqueue_1m_jitter", "enqueue", 60, 45, 1),
    FlowSpec("enqueue_5m_7m", "enqueue", 300, 420, 1),
    FlowSpec("enqueue_5m_6m_cap2", "enqueue", 300, 360, 2),
    FlowSpec("enqueue_10m_12m", "enqueue", 600, 720, 1),
    FlowSpec("skip_1m_90s", "skip", 60, 90, 1),
    FlowSpec("skip_1m_30s", "skip", 60, 30, 1, control=True),
    FlowSpec("skip_5m_7m", "skip", 300, 420, 1),
    FlowSpec("skip_5m_8m_cap2", "skip", 300, 480, 2, control=True),
    FlowSpec("skip_10m_15m", "skip", 600, 900, 1),
    FlowSpec("cancel_1m_90s", "cancel_new", 60, 90, 1),
    FlowSpec("cancel_1m_165s", "cancel_new", 60, 165, 1),  # not a multiple of the interval: the run's overhead must not push it past the third tick
    FlowSpec("cancel_5m_7m", "cancel_new", 300, 420, 1),
    FlowSpec("cancel_5m_4m", "cancel_new", 300, 240, 1, control=True),
    FlowSpec("cancel_10m_12m_cap2", "cancel_new", 600, 720, 2, control=True),
)
JITTER_FLOW = "enqueue_1m_jitter"


def build_manifest(seed: int, quick: bool) -> list[FlowSpec]:
    """The manifest for this seed: seeded offsets within each interval and the jitter list."""
    rng = random.Random(seed)
    specs = []
    for spec in MANIFEST:
        if spec.name == JITTER_FLOW:
            spec = replace(spec, durations=tuple(round(rng.uniform(15, 75), 1) for _ in range(12)))
        if quick:
            spec = spec.scaled(QUICK_SCALE)
        # Leave a little room at the start so the first fire never lands on server warm-up.
        spec = replace(spec, offset=round(rng.uniform(2.0, spec.interval - 2.0), 1))
        specs.append(spec)
    return specs


# -- model -------------------------------------------------------------------


def ticks(spec: FlowSpec, window: float) -> list[float]:
    """Fire times (seconds from window start) inside ``[0, window)``."""
    offset = spec.offset or 0.0
    out = []
    t = offset
    while t < window:
        out.append(t)
        t += spec.interval
    return out


def simulate(spec: FlowSpec, window: float) -> dict:
    """What the policy should produce over ``window`` seconds.

    Returns started, completed (ended inside the window), skipped, cancelled,
    and pending (never started inside the window)."""
    started = completed = skipped = cancelled = pending = 0
    ends: list[float] = []
    for i, t in enumerate(ticks(spec, window)):
        dur = spec.duration_at(i)
        ends = [e for e in ends if e > t]
        if spec.policy == "enqueue":
            if len(ends) >= spec.cap:
                start = min(ends)
                ends.remove(start)
            else:
                start = t
            ends.append(start + dur)  # a run that starts after the window still holds the slot for those behind it
            if start >= window:
                pending += 1
                continue
            started += 1
            if start + dur <= window:
                completed += 1
        else:
            if len(ends) >= spec.cap:
                if spec.policy == "skip":
                    skipped += 1
                else:
                    cancelled += 1
                continue
            started += 1
            ends.append(t + dur)
            if t + dur <= window:
                completed += 1
    return {
        "fires": len(ticks(spec, window)),
        "started": started,
        "completed": completed,
        "skipped": skipped,
        "cancelled": cancelled,
        "pending": pending,
    }


# -- generated project ---------------------------------------------------------

PIPELINE = '''"""Generated by benches/soak_overlap.py (seed {seed}). Do not edit."""

import re
import time
from datetime import datetime, timezone

from cereyan import App, Interval, get_run_logger, task
from cereyan.context import current_run

app = App("{project}")

SPECS = {specs}

_STAMP = re.compile(r"(\\d{{8}}T\\d{{6}})")


@task
def phase(label: str, seconds: float) -> float:
    get_run_logger().info("phase %s: sleeping %.1fs", label, seconds)
    time.sleep(seconds)
    return seconds


def _duration(spec) -> float:
    if not spec["durations"]:
        return spec["duration"]
    run = current_run()
    m = _STAMP.search(run.name if run is not None else "")
    if m is None:
        return spec["durations"][0]
    scheduled = datetime.strptime(m.group(1), "%Y%m%dT%H%M%S").replace(tzinfo=timezone.utc)
    anchor = datetime.fromisoformat(spec["anchor"])
    tick = round((scheduled - anchor).total_seconds() / spec["interval"])
    return spec["durations"][tick % len(spec["durations"])]


def _make(spec):
    def body() -> float:
        log = get_run_logger()
        seconds = _duration(spec)
        log.info("start %s: duration %.1fs, cap %d, policy %s", spec["name"], seconds, spec["cap"], spec["policy"])
        phase("first", seconds / 2)
        phase("second", seconds / 2)
        log.info("end %s", spec["name"])
        return seconds

    body.__name__ = body.__qualname__ = spec["name"]
    return body


for _spec in SPECS:
    app.flow(
        _make(_spec),
        name=_spec["name"],
        tags=["soak", _spec["policy"]],
        schedule=Interval(_spec["interval"], anchor=datetime.fromisoformat(_spec["anchor"])),
        max_concurrent=_spec["cap"],
        on_overlap=_spec["policy"],
    )
'''

TOML = """[server]
max_engines = 24
"""


def write_project(directory: str, specs: list[FlowSpec], base: float, seed: int) -> None:
    rows = []
    for s in specs:
        anchor = datetime.fromtimestamp(base + (s.offset or 0.0), tz=timezone.utc)
        rows.append(
            {
                "name": s.name,
                "policy": s.policy,
                "interval": s.interval,
                "duration": s.duration,
                "durations": list(s.durations),
                "cap": s.cap,
                "anchor": anchor.isoformat(),
            }
        )
    os.makedirs(directory, exist_ok=True)
    with open(os.path.join(directory, "pipeline.py"), "w") as fh:
        fh.write(PIPELINE.format(seed=seed, project=PROJECT, specs=json.dumps(rows, indent=4)))
    with open(os.path.join(directory, "cereyan.toml"), "w") as fh:
        fh.write(TOML)


def print_manifest(specs: list[FlowSpec], minutes: float, seed: int, quick: bool) -> None:
    print(f"soak manifest: seed={seed} quick={quick} minutes={minutes:g}")
    print(f"{'flow':22} {'policy':10} {'interval':>9} {'duration':>9} {'cap':>3} {'offset':>7}  control")
    for s in specs:
        dur = f"{s.duration:.0f}s" if not s.durations else f"{min(s.durations):.0f}-{max(s.durations):.0f}s"
        print(f"{s.name:22} {s.policy:10} {s.interval:>8.0f}s {dur:>9} {s.cap:>3} {s.offset or 0:>6.1f}s  {'yes' if s.control else ''}")
    if any(s.durations for s in specs):
        j = next(s for s in specs if s.durations)
        print(f"{j.name} durations: {list(j.durations)}")


# -- server and sampling ---------------------------------------------------------


def rss_kb(pids: list[int]) -> int | None:
    if not pids or sys.platform.startswith("win"):
        return None
    try:
        out = subprocess.run(["ps", "-o", "rss=", "-p", ",".join(str(p) for p in pids)], capture_output=True, text=True, timeout=5)
    except Exception:
        return None
    return sum(int(x) for x in out.stdout.split() if x.isdigit()) or None


def sample(url: str, home: str | None) -> dict:
    t0 = time.perf_counter()
    api(url, "/api/runs?limit=50")
    runs_ms = (time.perf_counter() - t0) * 1000
    server = api(url, "/api/server")
    engine_pids = [e["pid"] for e in server.get("engines", []) if e.get("pid")]
    db = os.path.join(home or server.get("home", ""), "db.sqlite")
    return {
        "t": time.time(),
        "engines": len(server.get("engines", [])),
        "queued": server.get("queued"),
        "counts": api(url, "/api/counts"),
        "rss_kb": rss_kb([server["pid"]]),
        "engines_rss_kb": rss_kb(engine_pids),
        "db_bytes": os.path.getsize(db) if os.path.exists(db) else None,
        "runs_list_ms": round(runs_ms, 2),
    }


def all_runs(url: str, flow: str) -> list[dict]:
    items: list[dict] = []
    cursor = None
    while True:
        path = f"/api/runs?flow={flow}&project={PROJECT}&limit=500" + (f"&cursor={cursor}" if cursor else "")
        page = api(url, path)
        items.extend(page["items"])
        cursor = page.get("next_cursor")
        if not cursor:
            return items


# -- invariants ----------------------------------------------------------------


def us(seconds: float) -> int:
    return int(seconds * 1_000_000)


def max_concurrent(runs: list[dict], now_us: int) -> int:
    events = []
    for r in runs:
        if r["start_time"] is None:
            continue
        events.append((r["start_time"], 1))
        events.append((r["end_time"] if r["end_time"] is not None else now_us, -1))
    events.sort(key=lambda e: (e[0], e[1]))  # an end at the same instant frees before the start counts
    peak = cur = 0
    for _, d in events:
        cur += d
        peak = max(peak, cur)
    return peak


def found_free_slot(run: dict, runs: list[dict], cap: int) -> bool:
    sched = run["scheduled_time"]
    busy = sum(
        1
        for o in runs
        if o["id"] != run["id"]
        and o["start_time"] is not None
        and o["start_time"] <= sched
        and (o["end_time"] is None or o["end_time"] > sched)
    )
    return busy < cap


def percentile(values: list[float], pct: float) -> float | None:
    if not values:
        return None
    values = sorted(values)
    k = min(len(values) - 1, max(0, round(pct / 100 * (len(values) - 1))))
    return values[k]


def check_flow(spec: FlowSpec, runs: list[dict], window: float, pending_at_end: int, driver_cancelled: set[int], now_us: int) -> dict:
    """Observed numbers and the result of every applicable invariant for one flow."""
    model = simulate(spec, window)
    started = [r for r in runs if r["start_time"] is not None]
    skipped = [r for r in runs if r["state"]["name"] == "Skipped"]
    policy_cancelled = [r for r in runs if r["state"]["type"] == "Cancelled" and r["id"] not in driver_cancelled]
    completed = [r for r in runs if r["state"]["type"] == "Completed"]
    peak = max_concurrent(runs, now_us)
    overruns = [
        (r["end_time"] - r["start_time"]) / 1000 - spec.duration * 1000
        for r in completed
        if r["start_time"] is not None and r["end_time"] is not None and not spec.durations
    ]
    drifts = [(r["start_time"] - r["scheduled_time"]) / 1000 for r in started if found_free_slot(r, runs, spec.cap)]
    p95 = percentile(drifts, 95)
    results: dict[str, str] = {}

    results["1_fire_count"] = "ok" if abs(len(runs) - model["fires"]) <= 1 else f"{len(runs)} runs created, model says {model['fires']}"
    results["2_cap"] = "ok" if peak <= spec.cap else f"{peak} concurrent runs, cap is {spec.cap}"
    if spec.control:
        bad = len(skipped) + len(policy_cancelled)
        results["3_control_clean"] = "ok" if bad == 0 else f"control flow has {len(skipped)} skipped and {len(policy_cancelled)} cancelled runs"
    if spec.policy in ("skip", "cancel_new") and not spec.control:
        observed = len(skipped) if spec.policy == "skip" else len(policy_cancelled)
        expected = model["skipped"] if spec.policy == "skip" else model["cancelled"]
        results["4_policy_count"] = "ok" if abs(observed - expected) <= 2 else f"{observed} {spec.policy} runs, model says {expected}"
    if spec.policy in ("skip", "cancel_new"):
        wrong = [r["id"] for r in skipped + policy_cancelled if r["state"].get("message") != MESSAGE]
        results["5_message"] = "ok" if not wrong else f"runs {wrong[:5]} lack the message {MESSAGE!r}"
    if spec.policy == "enqueue":
        gaps = []
        for r in started:
            if r["start_time"] - r["scheduled_time"] <= us(2.0):
                continue
            prev_ends = [o["end_time"] for o in runs if o["end_time"] is not None and o["end_time"] <= r["start_time"] and o["id"] != r["id"]]
            if prev_ends:
                gaps.append((r["id"], (r["start_time"] - max(prev_ends)) / 1e6))
        late = [(rid, g) for rid, g in gaps if g >= 2.0]
        results["6_release_gap"] = "ok" if not late else f"run {late[0][0]} started {late[0][1]:.1f}s after the previous run ended"
        ordered = sorted(started, key=lambda r: r["scheduled_time"])
        out_of_order = [
            (a["id"], b["id"]) for a, b in zip(ordered, ordered[1:]) if b["start_time"] < a["start_time"]
        ]
        results["7_fifo"] = "ok" if not out_of_order else f"run {out_of_order[0][1]} started before run {out_of_order[0][0]}"
        results["8_backlog"] = "ok" if abs(pending_at_end - model["pending"]) <= 2 else f"{pending_at_end} pending at the end, model says {model['pending']}"
    results["9_drift"] = "ok" if p95 is None or p95 < 500 else f"p95 drift {p95:.0f} ms"

    return {
        "policy": spec.policy,
        "interval": spec.interval,
        "duration": spec.duration,
        "cap": spec.cap,
        "control": spec.control,
        "observed": {
            "created": len(runs),
            "started": len(started),
            "completed": len(completed),
            "skipped": len(skipped),
            "cancelled": len(policy_cancelled),
            "pending_at_end": pending_at_end,
            "max_concurrent": peak,
            "p95_drift_ms": p95,
            "max_drift_ms": max(drifts) if drifts else None,
            "median_overrun_ms": percentile(overruns, 50),
            "max_overrun_ms": max(overruns) if overruns else None,
        },
        "predicted": model,
        "invariants": results,
        "failed": sorted(k for k, v in results.items() if v != "ok"),
    }


def print_table(report: dict) -> None:
    print()
    print(f"{'flow':22} {'policy':10} {'int':>5} {'dur':>5} {'cap':>3} {'created':>7} {'done':>5} {'skip':>5} {'cancel':>6} {'pend':>5} {'maxc':>4} {'p95drift':>9}  status")
    for name, f in report["flows"].items():
        o = f["observed"]
        p95 = f"{o['p95_drift_ms']:.0f}ms" if o["p95_drift_ms"] is not None else "-"
        status = "ok" if not f["failed"] else "FAIL " + ",".join(f["failed"])
        print(
            f"{name:22} {f['policy']:10} {f['interval']:>4.0f}s {f['duration']:>4.0f}s {f['cap']:>3} {o['created']:>7} {o['completed']:>5} "
            f"{o['skipped']:>5} {o['cancelled']:>6} {o['pending_at_end']:>5} {o['max_concurrent']:>4} {p95:>9}  {status}"
        )
    for name, f in report["flows"].items():
        for inv in f["failed"]:
            print(f"  {name}: {inv}: {f['invariants'][inv]}")


# -- driver ----------------------------------------------------------------------


def wait_until(predicate, timeout: float, every: float = 0.5) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(every)
    return predicate()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--minutes", type=float, default=60.0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--quick", action="store_true", help=f"scale intervals and durations by 1/{QUICK_SCALE} and run {QUICK_MINUTES} minutes")
    ap.add_argument("--keep", action="store_true", help="leave the server on port 4200 with the UI open after the checks")
    ap.add_argument("--url", help="check an already running server instead of starting one")
    args = ap.parse_args()
    sys.stdout.reconfigure(line_buffering=True)  # progress shows when piped to a file
    minutes = QUICK_MINUTES if args.quick and args.minutes == 60.0 else args.minutes
    window = minutes * 60
    specs = build_manifest(args.seed, args.quick)
    print_manifest(specs, minutes, args.seed, args.quick)

    home = srv = None
    if args.url:
        url = args.url.rstrip("/")
        base = time.time()
        specs = [replace(s, offset=None) for s in specs]  # anchors unknown; fire count uses floor(window / interval)
    else:
        home = tempfile.mkdtemp(prefix="cereyan-soak-")
        proj = os.path.join(home, "proj")
        base = time.time() + 30.0
        write_project(proj, specs, base, args.seed)
        print(f"home: {home}")
        srv = Server(home, proj, port=4200 if args.keep else 0, open_browser=args.keep)
        url = srv.url
    print(f"server: {url}")
    flows = {f["name"]: f for f in api(url, f"/api/flows?project={PROJECT}")}
    missing = [s.name for s in specs if s.name not in flows]
    if missing:
        print(f"flows missing on the server: {missing}", file=sys.stderr)
        if srv:
            srv.stop()
        return 2
    for s in specs:
        if flows[s.name].get("error"):
            print(f"flow {s.name} has an error: {flows[s.name]['error']}", file=sys.stderr)

    end = base + window
    samples: list[dict] = []
    gaps: list[list[float]] = []
    print(f"window: {datetime.fromtimestamp(base, tz=timezone.utc):%H:%M:%S} to {datetime.fromtimestamp(end, tz=timezone.utc):%H:%M:%S} UTC ({minutes:g} min)")
    last = time.time()
    next_status = base + 60
    try:
        while time.time() < end:
            samples.append(sample(url, home))
            now = samples[-1]["t"]
            if now - last > GAP_WARN_SECS:
                print(f"warning: {now - last:.0f}s between samples (machine suspended or server stalled)", file=sys.stderr)
                gaps.append([last, now])
                samples[-1]["gap"] = True
            last = now
            if now >= next_status:
                c = samples[-1]["counts"]
                r = c.get("runs", {})
                print(
                    f"  {datetime.now(tz=timezone.utc):%H:%M:%S} engines={samples[-1]['engines']} queued={samples[-1]['queued']} "
                    f"running={r.get('Running')} completed={r.get('Completed')} cancelled={r.get('Cancelled')} "
                    f"server_rss={samples[-1]['rss_kb']}KB engines_rss={samples[-1]['engines_rss_kb']}KB db={samples[-1]['db_bytes']}B"
                )
                next_status += 60
            time.sleep(max(0.0, SAMPLE_SECS - (time.time() - now)))
    except KeyboardInterrupt:
        print("interrupted; checking what ran so far", file=sys.stderr)
        end = time.time()
        window = end - base

    # Freeze the picture: backlog first, then pause every schedule and cancel what is left.
    ws, we = us(base), us(end)
    in_window = {}
    pending_at_end = {}
    for s in specs:
        rs = [r for r in all_runs(url, s.name) if r["scheduled_time"] is not None and ws <= r["scheduled_time"] < we]
        in_window[s.name] = rs
        pending_at_end[s.name] = sum(1 for r in rs if r["state"]["type"] not in TERMINAL and r["start_time"] is None)
    # Pausing a schedule deletes its unstarted runs, which would erase the backlog from history,
    # so cancel in-window runs directly: unstarted first, so a freed slot does not start one.
    driver_cancelled: set[int] = set()
    for s in specs:
        live = [r for r in in_window[s.name] if r["state"]["type"] not in TERMINAL]
        for r in sorted(live, key=lambda r: r["start_time"] is not None):
            try:
                api(url, f"/api/runs/{r['id']}/cancel", {})
                driver_cancelled.add(r["id"])
            except RuntimeError as exc:
                print(f"cancel {r['id']}: {exc}", file=sys.stderr)

    def quiet() -> bool:
        return all(r["state"]["type"] in TERMINAL for s in specs for r in all_runs(url, s.name) if r["scheduled_time"] and ws <= r["scheduled_time"] < we)

    if not wait_until(quiet, 90):
        print("warning: some runs did not reach a terminal state after cancellation", file=sys.stderr)

    now_us = us(time.time())
    report = {
        "seed": args.seed,
        "quick": args.quick,
        "minutes": minutes,
        "window": [base, end],
        "url": url,
        "home": home,
        "manifest": [s.__dict__ for s in specs],
        "flows": {},
        "samples": samples,
        "gaps": gaps,
    }
    for s in specs:
        rs = [r for r in all_runs(url, s.name) if r["scheduled_time"] is not None and ws <= r["scheduled_time"] < we]
        report["flows"][s.name] = check_flow(s, rs, window, pending_at_end[s.name], driver_cancelled, now_us)
    failed = [n for n, f in report["flows"].items() if f["failed"]]
    report["failed"] = failed
    with open(REPORT, "w") as fh:
        json.dump(report, fh, indent=1, default=str)
    print_table(report)
    rss = [x["rss_kb"] for x in samples if x.get("rss_kb")]
    if rss:
        eng = [x["engines_rss_kb"] or 0 for x in samples]
        lat = [x["runs_list_ms"] for x in samples]
        print(
            f"\nserver rss: first {rss[0]} KB, last {rss[-1]} KB, peak {max(rss)} KB; engines rss peak {max(eng)} KB; "
            f"runs list p95 {percentile(lat, 95):.1f} ms; samples {len(samples)}, gaps {len(gaps)}"
        )
    print(f"report: {REPORT}")

    if srv is not None:
        if args.keep:
            print(f"\n--keep: server stays up at {url} (home {home}); schedules keep firing, the in-window backlog was cancelled. Ctrl-C to stop.")
            try:
                srv.proc.wait()
            except KeyboardInterrupt:
                srv.stop()
        else:
            srv.stop()
            shutil.rmtree(home, ignore_errors=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
