"""1.2 human-in-the-loop: wait_for_input pauses, resume replays with the answer."""

from __future__ import annotations

import io
import os
import time

import pytest

from cereyan import App, wait_for_input
from cereyan.client import ApiError
from cereyan.exceptions import CereyanError
from server_helpers import ServerProcess

PIPELINE = '''
import os, time
from cereyan import App, task, wait_for_input, INPUTS

app = App("hitl")
TRACE = os.environ["HITL_TRACE"]

def note(line):
    with open(TRACE, "a") as fh:
        fh.write(line + "\\n")

@task(cache=INPUTS, persist_result=True)
def prepare(n: int):
    note(f"prepare:{n}")
    return n * 2

@task
def apply_decision(value: int, decision: dict):
    note(f"apply:{value}:{decision['approve']}")
    return value if decision.get("approve") else 0

@app.flow
def approve(n: int = 2):
    value = prepare(n)
    decision = wait_for_input("Approve the load?", schema={"type": "object", "properties": {"approve": {"type": "boolean"}}})
    return apply_decision(value, decision)

@task
def prepare_plain(n: int):
    note(f"plain:{n}")
    return n * 3

@app.flow
def approve_plain(n: int = 2):
    value = prepare_plain(n)
    decision = wait_for_input("Approve?")
    return apply_decision(value, {"approve": bool(decision)})

@task(resources={"db": 1})
def hold():
    note("hold")
    return 1

@app.flow
def holder():
    hold()
    wait_for_input("Continue?")
    return "done"

@app.flow
def other():
    hold()
    return "other"

@app.flow
def two_questions():
    a = wait_for_input("First?")
    note(f"first:{a}")
    b = wait_for_input("Second?")
    note(f"second:{b}")
    return [a, b]

@app.flow
def same_prompt():
    a = wait_for_input("Are you sure?")
    b = wait_for_input("Are you sure?")
    return [a, b]

@task
def asks():
    return wait_for_input("From a task?")

@app.flow
def asks_from_a_task():
    return asks.submit().result()

from cereyan import receive, publish_state

@app.flow
def signal_flow():
    value = receive("signal")
    note(f"signal:{value}")
    return value

@task
def slow_until_go():
    note("slow:waiting")
    while not os.path.exists(TRACE + ".go"):
        time.sleep(0.02)
    return 1

@app.flow
def signal_while_running():
    slow_until_go.submit()
    value = receive("signal")
    note(f"late:{value}")
    return value

@app.flow
def publish_and_complete():
    publish_state("progress", {"step": 1})
    publish_state("progress", {"step": 2})
    return "done"

@app.flow
def timeout_flow():
    value = receive("signal", timeout=1, default="defaulted")
    note(f"timeout:{value}")
    return value

@app.flow
def odd_topic_flow():
    value = receive("a/b x+y#c&d=e")
    note(f"odd:{value}")

@app.flow
def timeout_values():
    none = receive("a", timeout=0.5, default=None)
    number = receive("b", timeout=0.5, default=42)
    note(f"defaults:{none!r}:{number!r}")

def crash_once(tag):
    marker = TRACE + "." + tag
    if not os.path.exists(marker):
        open(marker, "w").close()
        # Let the engine's batched task updates reach the server first.
        time.sleep(0.5)
        os._exit(7)

@task
def release(decision):
    note(f"release:{decision}")
    return decision

@app.flow(crash_retries=1)
def approve_then_crash():
    decision = wait_for_input("Approve the load?")
    released = release(decision)
    crash_once("approve")
    note(f"answer:{decision}")
    return released

@app.flow(crash_retries=1)
def two_then_crash():
    a = wait_for_input("First?")
    note(f"first:{a}")
    crash_once("two")
    b = wait_for_input("Second?")
    note(f"second:{b}")
    return [a, b]

@app.flow(crash_retries=1)
def prompt_changes_after_crash():
    prompt = "Publish to production?" if os.path.exists(TRACE + ".prompt") else "Approve the load?"
    answer = wait_for_input(prompt)
    crash_once("prompt")
    return answer

@app.flow(crash_retries=1)
def queued_then_crash():
    if not os.path.exists(TRACE + ".queued"):
        note("waiting")
        while not os.path.exists(TRACE + ".go"):
            time.sleep(0.02)
        crash_once("queued")
    value = receive("go")
    note(f"queued:{value}")
    return value

@app.flow(crash_retries=1)
def crash_then_receive():
    crash_once("first")
    value = receive("go")
    note(f"late:{value}")
    return value

@app.flow(crash_retries=1)
def always_crashes():
    os._exit(7)

@app.flow
def answer_then_fail():
    answer = wait_for_input("Approve?")
    note(f"approved:{answer}")
    if not os.path.exists(TRACE + ".failed"):
        open(TRACE + ".failed", "w").close()
        raise RuntimeError("failed after the approval")
    return answer

@app.flow(crash_retries=1)
def receive_then_crash():
    value = receive("go", timeout=5, default="defaulted")
    note(f"got:{value}")
    crash_once("receive")
    return value
'''


@pytest.fixture
def hitl(isolated_home, tmp_path):
    from cereyan import engine

    engine.close_store()
    d = tmp_path / "hitl"
    d.mkdir()
    (d / "pipeline.py").write_text(PIPELINE)
    (d / "cereyan.toml").write_text("[resources]\ndb = 1\n")
    trace = tmp_path / "trace.txt"
    trace.write_text("")
    srv = ServerProcess(str(isolated_home), str(d), env={"HITL_TRACE": str(trace), "CEREYAN_FAST_CRASH_RERUN": "1"}, extra=["--max-engines", "1"])
    srv.trace = trace
    try:
        yield srv
    finally:
        srv.stop()


def lines(srv):
    return [ln for ln in srv.trace.read_text().splitlines() if ln]


def start(srv, flow, **params):
    fid = next(f["id"] for f in srv.client.flows() if f["name"] == flow and f["project"] == "hitl")
    return srv.client._request("POST", f"/api/flows/{fid}/runs", body={"parameters": params})


def paused(srv, run_id):
    return srv.wait_run(run_id, until=lambda r: r["state"]["type"] == "Paused")


def test_pause_resume_replays_cached_prefix(hitl):
    run = start(hitl, "approve", n=3)
    p = paused(hitl, run["id"])
    assert p["state"]["details"]["prompt"] == "Approve the load?"
    assert p["state"]["details"]["schema"]["properties"]["approve"]["type"] == "boolean"
    assert p["state"]["message"] == "Approve the load?"
    assert lines(hitl) == ["prepare:3"]
    events = hitl.client._request("GET", f"/api/events?run_id={run['id']}&name=run.paused")["items"]
    assert len(events) == 1
    # The single engine is free while the run waits.
    done = hitl.client._request("GET", f"/api/runs/{run['id']}")
    assert done["engine_pid"] is None or hitl.client._request("GET", "/api/server")["engines"]
    # Resume with the answer: prepare is served from cache, the decision is applied.
    with pytest.raises(ApiError) as err:
        hitl.client.resume(run["id"], {"approve": True})
        hitl.client.resume(run["id"], {"approve": True})
    assert err.value.status == 409
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    assert lines(hitl) == ["prepare:3", "apply:6:True"]
    assert hitl.client._request("GET", f"/api/events?run_id={run['id']}&name=run.resumed")["items"]
    # A cancelled paused run drops its stored answer.
    again = start(hitl, "approve", n=4)
    paused(hitl, again["id"])
    hitl.client.cancel(again["id"])
    assert hitl.wait_run(again["id"])["state"]["type"] == "Cancelled"
    assert hitl.client._request("GET", f"/api/runs/{again['id']}/input")["input"] is None


def test_paused_run_releases_engine_and_resources(hitl):
    held = start(hitl, "holder")
    paused(hitl, held["id"])
    # With one engine and one `db`, another run can still proceed.
    o = start(hitl, "other")
    assert hitl.wait_run(o["id"])["state"]["type"] == "Completed"
    assert lines(hitl) == ["hold", "hold"]
    counts = hitl.client._request("GET", "/api/counts")
    assert counts["active"] >= 1
    hitl.client.resume(held["id"], "go")
    assert hitl.wait_run(held["id"])["state"]["type"] == "Completed"


def test_resume_records_the_task_runs_of_its_pass(hitl):
    run = start(hitl, "approve", n=3)
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], {"approve": True})
    assert hitl.wait_run(run["id"])["state"]["type"] == "Completed"
    tasks = hitl.client._request("GET", f"/api/runs/{run['id']}/tasks")
    by_pass: dict[int, list[str]] = {}
    for t in tasks:
        by_pass.setdefault(t["pass"], []).append(t["dynamic_key"])
    # The first execution recorded `prepare-0`. The resumed one is a new pass:
    # it records its replay of that same call and the work that follows, which
    # used to be dropped because the engine restarted the report sequence.
    assert by_pass[0] == ["prepare-0"]
    assert sorted(by_pass[1]) == ["apply_decision-0", "prepare-0"]
    first = hitl.client._request("GET", f"/api/runs/{run['id']}/tasks?pass=0")
    assert [t["dynamic_key"] for t in first] == ["prepare-0"]
    events = hitl.client._request("GET", f"/api/events?run_id={run['id']}&name=task_run.*")["items"]
    assert any(e["payload"].get("dynamic_key") == "apply_decision-0" for e in events)
    # The graph is one pass, so no edge joins task runs that never ran together.
    graph = hitl.client._request("GET", f"/api/runs/{run['id']}/graph")
    assert sorted(n["dynamic_key"] for n in graph["nodes"]) == ["apply_decision-0", "prepare-0"]


def test_resume_needs_paused_state(hitl):
    o = start(hitl, "other")
    hitl.wait_run(o["id"])
    with pytest.raises(ApiError) as err:
        hitl.client.resume(o["id"], {"x": 1})
    assert err.value.status == 409


def test_two_questions_each_wait_for_their_own_answer(hitl):
    run = start(hitl, "two_questions")
    first = paused(hitl, run["id"])
    assert first["state"]["message"] == "First?"
    assert first["state"]["details"]["index"] == 0
    hitl.client.resume(run["id"], "A")
    second = hitl.wait_run(
        run["id"],
        until=lambda r: r["state"]["type"] == "Paused" and r["state"]["message"] == "Second?",
    )
    assert second["state"]["details"]["index"] == 1
    hitl.client.resume(run["id"], "B")
    assert hitl.wait_run(run["id"])["state"]["type"] == "Completed"
    # The body replays from the top on each resume, so the first question is
    # reached again and answered from the store; the second gets its own
    # answer instead of the first one.
    assert lines(hitl) == ["first:A", "first:A", "second:B"]


def test_two_questions_with_the_same_prompt_both_pause(hitl):
    run = start(hitl, "same_prompt")
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], "yes")
    again = hitl.wait_run(
        run["id"],
        until=lambda r: r["state"]["type"] == "Paused" and r["state"]["details"].get("index") == 1,
    )
    assert again["state"]["message"] == "Are you sure?"
    hitl.client.resume(run["id"], "yes again")
    assert hitl.wait_run(run["id"])["state"]["type"] == "Completed"


def test_asking_from_a_task_is_refused(hitl):
    run = start(hitl, "asks_from_a_task")
    done = hitl.wait_run(run["id"])
    assert done["state"]["type"] == "Failed"
    assert "flow body" in (done["state"]["message"] or "")


def test_answers_are_dropped_when_the_run_ends(hitl):
    run = start(hitl, "two_questions")
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], "A")
    hitl.wait_run(
        run["id"],
        until=lambda r: r["state"]["type"] == "Paused" and r["state"]["details"].get("index") == 1,
    )
    hitl.client.resume(run["id"], "B")
    assert hitl.wait_run(run["id"])["state"]["type"] == "Completed"
    stored = hitl.client._request("GET", f"/api/runs/{run['id']}/input")
    assert stored["answers"] == {}
    assert stored["input"] is None and stored["pending"] is None


def test_a_changed_prompt_at_the_same_position_asks_again():
    from cereyan import context
    from cereyan.exceptions import RunPaused
    from cereyan.inputs import wait_for_input

    class Stub:
        offline = False

        def get_input(self, index):
            return {"prompt": "Approve the load?", "input": {"approve": True}}

        def claim_message(self, topic, index=0):
            stored = self.get_input(index)
            if stored is not None and stored.get("prompt") == "Approve the load?":
                return {"claimed": True, "answer": stored}
            return None

    ctx = context.RunContext(
        id=1, external_id="x", name="r", flow=None, parameters={}, backend=Stub()
    )
    token = context.set_run(ctx)
    try:
        assert wait_for_input("Approve the load?") == {"approve": True}
        # The same position, a different question: the stored answer is not
        # handed to it.
        ctx._input_index = 0
        with pytest.raises(RunPaused) as err:
            wait_for_input("Publish to production?")
        assert err.value.index == 0
    finally:
        context.reset_run(token)


def test_offline_terminal_and_non_tty(monkeypatch):
    app = App("hitl_offline")

    @app.flow
    def ask():
        return wait_for_input("Name?")

    class Tty(io.StringIO):
        def isatty(self):
            return True

    monkeypatch.setattr("sys.stdin", Tty("alice\n"))
    monkeypatch.setattr("builtins.input", lambda prompt="": "alice")
    assert ask() == "alice"
    monkeypatch.setattr("sys.stdin", io.StringIO(""))
    with pytest.raises(CereyanError) as err:
        ask()
    assert "Name?" in str(err.value)


def test_resume_replays_an_uncached_task_from_its_checkpoint(hitl):
    run = start(hitl, "approve_plain", n=5)
    paused(hitl, run["id"])
    assert lines(hitl) == ["plain:5"]
    hitl.client.resume(run["id"], True)
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    # prepare_plain has no cache policy; its checkpoint carried it across the pause.
    assert lines(hitl) == ["plain:5", "apply:15:True"]
    tasks = hitl.client._request("GET", f"/api/runs/{run['id']}/tasks")
    first = next(t for t in tasks if t["pass"] == 0 and t["dynamic_key"] == "prepare_plain-0")
    second = next(t for t in tasks if t["pass"] == 1 and t["dynamic_key"] == "prepare_plain-0")
    assert first["result_ref"] and first["input_hash"] and first["state"]["name"] == "Completed"
    assert second["state"]["name"] == "Replayed" and second["result_ref"] is None
    assert hitl.client._request("GET", f"/api/events?run_id={run['id']}&name=task_run.replayed")["items"]


def test_receive_topic_message_resumes_run(hitl):
    """receive('signal') pauses; POST /messages/{topic} resumes with the payload."""
    run = start(hitl, "signal_flow")
    p = paused(hitl, run["id"])
    assert p["state"]["details"]["topic"] == "signal"
    hitl.client.send_message(run["id"], "signal", {"x": 42})
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    assert lines(hitl)[-1] == "signal:{'x': 42}"


def test_resume_answers_a_receive_topic(hitl):
    """Resume answers the topic the run waits on, so receive('signal') completes."""
    run = start(hitl, "signal_flow")
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], {"x": 7})
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    assert lines(hitl)[-1] == "signal:{'x': 7}"


def test_message_sent_before_the_pause_lands_resumes_the_run(hitl):
    """A message sent while the run is still Running, after receive() missed it,
    is delivered once the run pauses on that topic."""
    import time

    run = start(hitl, "signal_while_running")
    deadline = time.time() + 30
    while "slow:waiting" not in lines(hitl):
        assert time.time() < deadline, "the task never started"
        time.sleep(0.02)
    # receive() has missed and the runner is waiting for the task: still Running.
    assert hitl.client.get_run(run["id"])["state"]["type"] == "Running"
    hitl.client.send_message(run["id"], "signal", {"x": 5})
    (hitl.trace.parent / (hitl.trace.name + ".go")).write_text("")
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    assert lines(hitl)[-1] == "late:{'x': 5}"


def test_publish_state_is_readable_via_api(hitl):
    """publish_state stores flow-level state readable from the API."""
    run = start(hitl, "publish_and_complete")
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    state = hitl.client._request("GET", f"/api/runs/{run['id']}/state")
    progress = next((s for s in state if s["scope"] == "" and s["key"] == "progress"), None)
    assert progress is not None
    assert progress["value"]["step"] == 2


def test_receive_timeout_uses_default(hitl):
    """receive(..., timeout=1, default='x') resumes with default when no message arrives."""
    run = start(hitl, "timeout_flow")
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    assert lines(hitl)[-1] == "timeout:defaulted"


def test_topics_with_url_characters_round_trip(hitl):
    """A topic with '/', '+', '#', '&', '=' and a space is claimed and sent intact."""
    run = start(hitl, "odd_topic_flow")
    p = paused(hitl, run["id"])
    assert p["state"]["details"]["topic"] == "a/b x+y#c&d=e"
    hitl.client.send_message(run["id"], "a/b x+y#c&d=e", 9)
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    assert lines(hitl)[-1] == "odd:9"


def test_receive_timeout_defaults_keep_their_type(hitl):
    """default=None comes back as None and default=42 as 42, not their JSON text."""
    run = start(hitl, "timeout_values")
    final = hitl.wait_run(run["id"])
    assert final["state"]["type"] == "Completed"
    assert lines(hitl)[-1] == "defaults:None:42"


def crash_rerun_of(srv, run_id):
    deadline = time.time() + 40
    while time.time() < deadline:
        for r in srv.client._request("GET", "/api/runs", params={"limit": 50})["items"]:
            if r["created_by"] == f"crash:{run_id}":
                return r
        time.sleep(0.1)
    raise AssertionError(f"no crash rerun of {run_id}\n{srv.read_log()}")


def settled(srv, run_id):
    return srv.wait_run(run_id, timeout=40, until=lambda r: r["state"]["type"] in ("Completed", "Failed", "Cancelled", "Crashed", "Paused"))


def test_a_crash_rerun_keeps_the_answer(hitl):
    run = start(hitl, "approve_then_crash")
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], "yes")
    assert hitl.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    rerun = crash_rerun_of(hitl, run["id"])
    final = settled(hitl, rerun["id"])
    assert final["state"]["type"] == "Completed", final["state"]
    # release ran once, before the crash; the rerun replayed it and its
    # wait_for_input returned the answer the crashed run held.
    assert lines(hitl) == ["release:yes", "answer:yes"]


def test_a_crash_rerun_keeps_a_consumed_message(hitl):
    run = start(hitl, "receive_then_crash")
    paused(hitl, run["id"])
    hitl.client.send_message(run["id"], "go", {"x": 1})
    assert hitl.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    rerun = crash_rerun_of(hitl, run["id"])
    final = settled(hitl, rerun["id"])
    assert final["state"]["type"] == "Completed", final["state"]
    assert lines(hitl) == ["got:{'x': 1}", "got:{'x': 1}"]


def answers(srv, run_id):
    return srv.client._request("GET", f"/api/runs/{run_id}/input")["answers"]


def test_a_crash_rerun_asks_only_unanswered_questions(hitl):
    run = start(hitl, "two_then_crash")
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], "A")
    assert hitl.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    # The crashed run keeps its answer for the rerun.
    assert answers(hitl, run["id"])["0"]["input"] == "A"
    rerun = crash_rerun_of(hitl, run["id"])
    second = settled(hitl, rerun["id"])
    assert second["state"]["type"] == "Paused" and second["state"]["details"]["index"] == 1
    assert second["state"]["message"] == "Second?"
    # The rerun shows the answer it inherits, marked with the run holding it.
    inherited = answers(hitl, rerun["id"])
    assert inherited["0"]["input"] == "A" and inherited["0"]["run_id"] == run["id"]
    assert hitl.client._request("GET", f"/api/runs/{rerun['id']}/input", params={"index": 0})["answer"]["input"] == "A"
    hitl.client.resume(rerun["id"], "B")
    assert hitl.wait_run(rerun["id"])["state"]["type"] == "Completed"
    # The crashed pass, the rerun's first pass and its resumed pass each pass
    # the first question, answered "A" every time.
    assert lines(hitl) == ["first:A", "first:A", "first:A", "second:B"]
    # The chain has ended: no run of it keeps an answer.
    assert answers(hitl, run["id"]) == {}
    assert answers(hitl, rerun["id"]) == {}


def test_a_changed_prompt_asks_again_through_the_chain(hitl):
    run = start(hitl, "prompt_changes_after_crash")
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], "yes")
    assert hitl.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    rerun = crash_rerun_of(hitl, run["id"])
    again = settled(hitl, rerun["id"])
    assert again["state"]["type"] == "Paused"
    assert again["state"]["message"] == "Publish to production?"
    assert again["state"]["details"]["index"] == 0
    hitl.client.resume(rerun["id"], "no")
    final = hitl.wait_run(rerun["id"])
    assert final["state"]["type"] == "Completed"


def test_a_message_queued_for_the_crashed_run_reaches_the_rerun(hitl):
    run = start(hitl, "queued_then_crash")
    deadline = time.time() + 30
    while "waiting" not in lines(hitl):
        assert time.time() < deadline, "the flow never started"
        time.sleep(0.02)
    queued = hitl.client.send_message(run["id"], "go", {"x": 3})
    assert queued["id"] == run["id"]
    (hitl.trace.parent / (hitl.trace.name + ".go")).write_text("")
    assert hitl.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    rerun = crash_rerun_of(hitl, run["id"])
    final = settled(hitl, rerun["id"])
    assert final["state"]["type"] == "Completed", final["state"]
    assert lines(hitl)[-1] == "queued:{'x': 3}"


def test_a_message_sent_after_the_crash_reaches_the_chain(hitl):
    run = start(hitl, "crash_then_receive")
    assert hitl.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    # Whether the rerun exists yet or not, the message is not refused.
    sent = hitl.client.send_message(run["id"], "go", {"x": 4})
    rerun = crash_rerun_of(hitl, run["id"])
    assert sent["id"] in (run["id"], rerun["id"])
    final = settled(hitl, rerun["id"])
    assert final["state"]["type"] == "Completed", final["state"]
    assert lines(hitl) == ["late:{'x': 4}"]


def test_a_message_to_an_old_run_wakes_the_waiting_rerun(hitl):
    run = start(hitl, "crash_then_receive")
    assert hitl.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    rerun = crash_rerun_of(hitl, run["id"])
    waiting = settled(hitl, rerun["id"])
    assert waiting["state"]["type"] == "Paused" and waiting["state"]["details"]["topic"] == "go"
    sent = hitl.client.send_message(run["id"], "go", {"x": 5})
    assert sent["id"] == rerun["id"]
    assert hitl.wait_run(rerun["id"])["state"]["type"] == "Completed"
    assert lines(hitl) == ["late:{'x': 5}"]


def test_a_message_to_an_ended_chain_is_refused(hitl):
    run = start(hitl, "always_crashes")
    assert hitl.wait_run(run["id"], timeout=40)["state"]["type"] == "Crashed"
    rerun = crash_rerun_of(hitl, run["id"])
    last = hitl.wait_run(rerun["id"], timeout=40)
    assert last["state"]["type"] == "Failed" and last["state"]["message"] == "crash limit reached"
    with pytest.raises(ApiError) as err:
        hitl.client.send_message(run["id"], "go", 1)
    assert err.value.status == 409


def test_a_retry_from_failure_asks_again(hitl):
    run = start(hitl, "answer_then_fail")
    paused(hitl, run["id"])
    hitl.client.resume(run["id"], "yes")
    assert hitl.wait_run(run["id"])["state"]["type"] == "Failed"
    retry = hitl.client.retry(run["id"])["run"]
    again = paused(hitl, retry["id"])
    assert again["state"]["details"]["index"] == 0
    hitl.client.resume(retry["id"], "still yes")
    assert hitl.wait_run(retry["id"])["state"]["type"] == "Completed"
    assert lines(hitl) == ["approved:yes", "approved:still yes"]
