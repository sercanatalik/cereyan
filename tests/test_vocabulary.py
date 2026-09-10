"""The event and state name catalogue: the constants, and the validation that
rejects a rule which could never fire."""

from __future__ import annotations

import json
import sys
import time

import pytest

from cereyan import App
from cereyan.client import ApiError


@pytest.fixture(autouse=True)
def _isolated_rule_registry():
    """Rules declared here must not leak into other test files."""
    from cereyan import rules as rules_mod

    saved = dict(rules_mod._registry)
    yield
    rules_mod._registry.clear()
    rules_mod._registry.update(saved)


def make_rule(**when):
    body = {
        "name": "probe",
        "when": {"events": ["run.failed"], **when},
        "do": [{"kind": "cancel_run"}],
    }
    return body


# ---- served validation ----------------------------------------------------


def test_api_rejects_an_unknown_event(server):
    with pytest.raises(ApiError) as exc:
        server.client._request("POST", "/api/rules", body=make_rule(events=["run.failure"]))
    assert exc.value.status == 422
    assert "run.failure" in str(exc.value) and "run.failed" in str(exc.value)
    assert not [r for r in server.client._request("GET", "/api/rules") if r["name"] == "probe"]


def test_api_rejects_an_unknown_state(server):
    with pytest.raises(ApiError) as exc:
        server.client._request("POST", "/api/rules", body=make_rule(states=["Faild"]))
    assert exc.value.status == 422
    assert "Faild" in str(exc.value) and "Failed" in str(exc.value)
    assert not [r for r in server.client._request("GET", "/api/rules") if r["name"] == "probe"]


def test_api_rejects_an_unknown_name_in_the_unless_clause(server):
    body = make_rule(events=["run.running"])
    body["unless"] = {"events": ["run.complete"]}
    body["within"] = 60
    with pytest.raises(ApiError) as exc:
        server.client._request("POST", "/api/rules", body=body)
    assert exc.value.status == 422
    # The message says which clause is wrong, not just which name.
    assert "unless" in str(exc.value) and "run.completed" in str(exc.value)


def test_api_accepts_custom_and_wildcard_names(server):
    created = server.client._request(
        "POST",
        "/api/rules",
        body=make_rule(events=["orders.table_empty", "run.*"], states=["Failed", "TimedOut"]),
    )
    assert created["when"]["events"] == ["orders.table_empty", "run.*"]
    server.client._request("DELETE", f"/api/rules/{created['id']}")


def test_api_rejects_a_stream_message_name(server):
    """`run.updated` is an SSE message, not an event; a rule on it never fires."""
    with pytest.raises(ApiError) as exc:
        server.client._request("POST", "/api/rules", body=make_rule(events=["run.updated"]))
    assert exc.value.status == 422


# ---- constants ------------------------------------------------------------


def test_names_are_their_wire_strings():
    from cereyan import events, states

    assert events.run.failed == "run.failed"
    assert isinstance(events.run.failed, str)
    assert json.dumps({"on": events.run.failed}) == '{"on": "run.failed"}'
    assert states.Failed == "Failed"
    assert json.dumps([states.AwaitingRetry]) == '["AwaitingRetry"]'


def test_wildcard_attribute():
    from cereyan import events

    assert events.run.any == "run.*"
    assert events.task_run.any == "task_run.*"
    assert events.expectation.any == "expectation.*"


def test_leaf_with_a_dot_becomes_an_underscore():
    from cereyan import events

    assert events.rule.action_completed == "rule.action.completed"
    assert events.rule.action_failed == "rule.action.failed"
    assert events.rule.fired == "rule.fired"


def test_sub_states_carry_their_type():
    from cereyan import states

    assert states.AwaitingRetry.state_type == "Scheduled"
    assert states.AwaitingRetry.is_sub_state
    assert states.Skipped.state_type == "Completed"
    assert states.Scheduled.state_type == "Scheduled"
    assert not states.Scheduled.is_sub_state


def test_a_misspelled_constant_suggests_the_real_one():
    from cereyan import events, states

    with pytest.raises(ValueError, match="run.failed"):
        events.run.failure
    with pytest.raises(ValueError, match="Failed"):
        states.Faild


def test_python_constants_match_the_core_catalogue():
    """The binding cannot silently lag the core: this fails if a name is added
    to one side alone."""
    from cereyan import _core, events, states

    assert {str(n) for n in events.ALL} == {name for name, _, _, _ in _core.event_names()}
    assert events.RESERVED_PREFIXES == tuple(_core.reserved_prefixes())
    assert {str(s) for s in states.TYPES} == set(_core.state_types())
    assert {str(s) for s in states.SUB_STATES} == {n for n, _ in _core.state_names()}
    # Every catalogue name is reachable through its namespace.
    for name in events.ALL:
        group = getattr(events, name.resource if name.resource != "rule" else name.split(".")[0])
        assert getattr(group, str(name).split(".", 1)[1].replace(".", "_")) == name


# ---- the decorator --------------------------------------------------------


def test_decorator_rejects_a_misspelled_event():
    app = App("vocab")
    with pytest.raises(ValueError, match="run.failed"):

        @app.rule(on="run.failure")
        def r(event, run):
            pass


def test_decorator_rejects_a_misspelled_state():
    app = App("vocab")
    with pytest.raises(ValueError, match="Failed"):

        @app.rule(on="run.failed", states=["Faild"])
        def r(event, run):
            pass


def test_decorator_rejects_a_misspelled_event_in_unless():
    app = App("vocab")
    with pytest.raises(ValueError, match="run.completed"):

        @app.rule(on="run.running", unless="run.complete", within=60)
        def r(event, run):
            pass


def test_decorator_rejects_an_unrecognised_guard():
    """The old behaviour dropped it, leaving the rule running on defaults."""
    app = App("vocab")
    with pytest.raises(ValueError, match="cooldownseconds"):

        @app.rule(on="run.failed", cooldownseconds=60)
        def r(event, run):
            pass


def test_decorator_accepts_custom_events_and_wildcards():
    app = App("vocab")

    @app.rule(on=["orders.table_empty", "run.*"])
    def r(event, run):
        pass

    assert app.rules[-1].on == ["orders.table_empty", "run.*"]


def test_constants_and_strings_produce_the_same_spec():
    from cereyan import events, states

    app = App("vocab")

    @app.rule(on=events.run.failed, states=[states.Failed], name="r")
    def with_constants(event, run):
        pass

    @app.rule(on="run.failed", states=["Failed"], name="r")
    def with_strings(event, run):
        pass

    a, b = app.rules[-2].spec(), app.rules[-1].spec()
    a["do"] = b["do"] = None  # the callables differ; the match clause is the point
    assert a == b
    # Identical on the wire, which is what keeps the constants a pure Python
    # convenience: nothing downstream can tell them apart.
    assert json.dumps(a) == json.dumps(b)


def test_emit_event_refuses_the_engine_namespace(store):
    from cereyan import emit_event

    with pytest.raises(ValueError, match="reserved prefix"):
        emit_event("run.mine")
    emit_event("orders.table_empty", {"table": "orders"})


# ---- offline matching -----------------------------------------------------


@pytest.mark.xfail(
    sys.platform == "win32",
    reason="windows-interrupt-running-flow: timeout_seconds is a silent no-op on Windows",
    strict=True,
)
def test_offline_rule_on_a_state_type_matches_its_sub_state(store):
    """Offline and served share one matcher, so the widening applies to both.

    A timed-out run is Failed with the sub-state name TimedOut. Before this
    change `states=["Failed"]` did not match it, which is the bug the docstring
    always claimed was not there.
    """
    from cereyan import flow
    from cereyan.rules import register_with_store

    broad, narrow, wrong = [], [], []
    app = App("vocab_offline")

    @app.rule(on="run.failed", states=["Failed"], name="broad")
    def on_type(event, run):
        broad.append(run["state"]["name"])

    @app.rule(on="run.failed", states=["TimedOut"], name="narrow")
    def on_sub_state(event, run):
        narrow.append(run["state"]["name"])

    @app.rule(on="run.failed", states=["Cancelled"], name="wrong")
    def on_other_type(event, run):
        wrong.append(run["state"]["name"])

    @flow(app=app, timeout_seconds=0.2)
    def slow():
        time.sleep(5)

    register_with_store(store)
    with pytest.raises(TimeoutError):
        slow()

    assert broad == ["TimedOut"], "a rule on the type must see the sub-state"
    assert narrow == ["TimedOut"], "naming the sub-state still matches it"
    assert wrong == [], "an unrelated type must not match"
