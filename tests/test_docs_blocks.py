"""scripts/docs_blocks.py refuses parameter defaults computed at import."""

from __future__ import annotations

import importlib.util
import os

import pytest

SCRIPT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts", "docs_blocks.py")
spec = importlib.util.spec_from_file_location("docs_blocks", SCRIPT)
docs_blocks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(docs_blocks)


@pytest.mark.parametrize("default", ["date.today()", "datetime.now()", 'os.getenv("DAY")', "uuid4()", "date.today() - timedelta(days=1)"])
def test_a_computed_default_is_refused(default):
    found = docs_blocks.computed_defaults(f"def load(day={default}):\n    pass\n")
    assert found == [(1, "load", "day")]


@pytest.mark.parametrize("default", ["None", "2", '"x"', "date(2026, 9, 6)", "timedelta(hours=1)", "timedelta(days=-1)"])
def test_a_constant_default_passes(default):
    assert docs_blocks.computed_defaults(f"def load(day={default}):\n    pass\n") == []


def test_keyword_only_and_nested_functions_are_checked():
    source = "def outer():\n    def inner(*, at=time.time()):\n        pass\n"
    assert docs_blocks.computed_defaults(source) == [(2, "inner", "at")]


def test_a_problem_names_the_line_in_the_page():
    problems: list[str] = []
    docs_blocks.check_defaults("x = 1\ndef load(day=date.today()):\n    pass", "docs/page.md", 10, problems)
    assert len(problems) == 1 and problems[0].startswith("docs/page.md:12: `load` computes the default of `day`")
