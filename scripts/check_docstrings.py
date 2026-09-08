"""Fail when a public object of the ``cereyan`` package lacks a docstring, or when a
``Flow``/``Task`` constructor option is missing from the ``flow``/``task`` docstring.

Run from ``just lint`` and ``just docs``.
"""

from __future__ import annotations

import importlib
import inspect
import os
import re
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python"))

import cereyan  # noqa: E402

EXTRA_MODULES = [
    "cereyan.artifacts",
    "cereyan.client",
    "cereyan.schedules",
    "cereyan.targets",
    "cereyan.variables",
    "cereyan.runners",
    "cereyan.inputs",
    "cereyan.rules",
    "cereyan.routes",
    "cereyan.results",
    "cereyan.events",
    "cereyan.logging",
    "cereyan.exceptions",
]


def _methods(qualname: str, cls: type):
    for name, member in vars(cls).items():
        if name.startswith("_"):
            continue
        if isinstance(member, (staticmethod, classmethod)):
            member = member.__func__
        if isinstance(member, property):
            member = member.fget
        if inspect.isfunction(member):
            yield f"{qualname}.{name}", member


def public_objects() -> dict[str, object]:
    found: dict[str, object] = {}

    def add(qualname: str, obj) -> None:
        if qualname in found:
            return
        found[qualname] = obj
        if inspect.isclass(obj):
            for sub, member in _methods(qualname, obj):
                found.setdefault(sub, member)

    for name in cereyan.__all__:
        obj = getattr(cereyan, name)
        if inspect.isclass(obj) or inspect.isfunction(obj):
            add(f"cereyan.{name}", obj)
    for modname in EXTRA_MODULES:
        mod = importlib.import_module(modname)
        for name, obj in vars(mod).items():
            if name.startswith("_") or not (inspect.isclass(obj) or inspect.isfunction(obj)):
                continue
            if getattr(obj, "__module__", None) != modname:
                continue
            add(f"{modname}.{name}", obj)
    return found


def missing_docstrings() -> list[str]:
    return sorted(q for q, obj in public_objects().items() if not (inspect.getdoc(obj) or "").strip())


def missing_options() -> list[str]:
    from cereyan.flows import Flow
    from cereyan.tasks import Task

    problems = []
    for cls, decorator in ((Flow, cereyan.flow), (Task, cereyan.task)):
        doc = inspect.getdoc(decorator) or ""
        for name in inspect.signature(cls.__init__).parameters:
            if name in ("self", "fn"):
                continue
            if not re.search(rf"\b{re.escape(name)}\b", doc):
                problems.append(f"{decorator.__name__}: option `{name}` is not mentioned in its docstring")
    return problems


def main() -> int:
    problems = [f"no docstring: {q}" for q in missing_docstrings()] + missing_options()
    for line in problems:
        print(line)
    if problems:
        print(f"{len(problems)} problem(s)")
        return 1
    print(f"docstrings: {len(public_objects())} public objects documented")
    return 0


if __name__ == "__main__":
    sys.exit(main())
