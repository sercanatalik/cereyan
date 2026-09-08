"""The overlap soak's invariant model against hand-computed cases."""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "benches"))

from soak_overlap import MANIFEST, FlowSpec, build_manifest, simulate  # noqa: E402

HOUR = 3600.0


def test_alternating_skip():
    m = simulate(FlowSpec("s", "skip", 60, 90, 1, offset=0.0), HOUR)
    assert m["fires"] == 60
    assert (m["started"], m["skipped"]) == (30, 30)


def test_two_of_three_cancelled():
    m = simulate(FlowSpec("c", "cancel_new", 60, 165, 1, offset=0.0), HOUR)
    assert (m["started"], m["cancelled"]) == (20, 40)


def test_multiple_of_interval_is_a_boundary():
    # A duration equal to three intervals is the boundary the manifest avoids: any overhead flips it to one in four.
    m = simulate(FlowSpec("c", "cancel_new", 60, 180, 1, offset=0.0), HOUR)
    assert (m["started"], m["cancelled"]) == (20, 40)
    m = simulate(FlowSpec("c", "cancel_new", 60, 180.5, 1, offset=0.0), HOUR)
    assert (m["started"], m["cancelled"]) == (15, 45)


def test_control_never_skips():
    m = simulate(FlowSpec("s", "skip", 60, 30, 1, control=True, offset=0.0), HOUR)
    assert (m["started"], m["skipped"], m["completed"]) == (60, 0, 60)
    m = simulate(FlowSpec("s", "skip", 300, 480, 2, control=True, offset=0.0), HOUR)
    assert m["skipped"] == 0 and m["started"] == 12


def test_enqueue_backlog():
    # Run k starts at 90k, so ticks 40..59 never start inside the hour.
    m = simulate(FlowSpec("e", "enqueue", 60, 90, 1, offset=0.0), HOUR)
    assert (m["started"], m["pending"], m["skipped"], m["cancelled"]) == (40, 20, 0, 0)
    m = simulate(FlowSpec("e", "enqueue", 300, 360, 2, offset=0.0), HOUR)
    assert m["pending"] == 0 and m["started"] == 12


def test_offset_reduces_fires():
    m = simulate(FlowSpec("s", "skip", 60, 30, 1, offset=30.0), HOUR)
    assert m["fires"] == 60
    m = simulate(FlowSpec("s", "skip", 600, 30, 1, offset=30.0), HOUR)
    assert m["fires"] == 6


def test_manifest_shape():
    assert len(MANIFEST) == 15
    assert sum(1 for s in MANIFEST if s.control) == 4
    assert sum(1 for s in MANIFEST if s.duration > s.interval) == 12
    for policy in ("enqueue", "skip", "cancel_new"):
        assert sum(1 for s in MANIFEST if s.policy == policy) == 5
    a, b = build_manifest(7, False), build_manifest(7, False)
    assert a == b
    q = build_manifest(7, True)
    assert [s.interval for s in q] == [s.interval / 5 for s in a]
    assert all(s.durations for s in q if s.name == "enqueue_1m_jitter")
