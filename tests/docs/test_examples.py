"""Every file under ``examples/`` is a literate example: its header names the fixture it needs, and it must run to completion."""

from __future__ import annotations

import glob
import os
import runpy

import pytest
from cereyan import engine

from fixtures import EXAMPLES

FILES = sorted(glob.glob(os.path.join(EXAMPLES, "*.py")))


def header(path: str) -> dict[str, str]:
    meta: dict[str, str] = {}
    with open(path, encoding="utf-8") as fh:
        lines = fh.read().splitlines()
    if not lines or lines[0].strip() != "# ---":
        raise AssertionError(f"{path}: literate examples start with a '# ---' header")
    for line in lines[1:]:
        if line.strip() == "# ---":
            break
        key, _, value = line.lstrip("# ").partition(":")
        meta[key.strip()] = value.strip()
    for key in ("title", "description", "order"):
        assert key in meta, f"{path}: header lacks {key}"
    return meta


@pytest.mark.parametrize("path", FILES, ids=[os.path.basename(p) for p in FILES])
def test_example_runs(path, request, monkeypatch):
    meta = header(path)
    if meta.get("fixture") == "served":
        server = request.getfixturevalue("served")
        monkeypatch.setenv("CEREYAN_HOME", str(server.home))
        engine.configure(None)
        engine.close_store()
    runpy.run_path(path, run_name="__main__")
