"""Artifacts: markdown, tables, progress, links, and images attached to runs."""

from __future__ import annotations

import base64
import json
from typing import Any

from . import context
from .exceptions import CereyanError
from .params import to_json_value

MAX_BYTES = 1_048_576


def _backend():
    run = context.current_run()
    if run is None or run.backend is None:
        raise CereyanError("artifacts can only be created inside a flow or task run")
    task = context.current_task_run()
    return run.backend, (task.id if task is not None else None)


def _store(kind: str, data: Any, key: str | None) -> str:
    text = json.dumps(to_json_value(data))
    if len(text.encode("utf-8")) > MAX_BYTES:
        raise CereyanError(f"artifact exceeds 1 MB ({len(text)} bytes)")
    backend, task_ext = _backend()
    return backend.artifact(kind, text, key, task_ext)


def create_markdown(text: str, key: str | None = None) -> str:
    """Attach a markdown artifact to the current run or task run.

    Args:
        text: Markdown source, rendered on the run page.
        key: Optional stable key; artifacts sharing a key form a history across runs.

    Returns:
        The artifact id.

    Raises:
        CereyanError: Outside a run, or when the artifact exceeds 1 MB.
    """
    return _store("markdown", {"text": str(text)}, key)


def create_table(rows: list[dict] | list[list], key: str | None = None, columns: list[str] | None = None) -> str:
    """Attach a table artifact.

    Args:
        rows: A list of dicts (columns inferred from the keys, in first-seen order) or a
            list of lists (then pass ``columns``).
        key: Optional stable key.
        columns: Column names; required for list-of-lists rows.

    Returns:
        The artifact id.
    """
    rows = list(rows)
    if columns is None and rows and isinstance(rows[0], dict):
        seen: list[str] = []
        for r in rows:
            for k in r:
                if k not in seen:
                    seen.append(k)
        columns = seen
    return _store("table", {"columns": columns or [], "rows": rows}, key)


def create_progress(percent: float, key: str | None = None, label: str | None = None) -> str:
    """Attach a progress bar artifact.

    Args:
        percent: Completion from 0 to 100; values outside are clamped.
        key: Optional stable key; pass the same key to `update_progress` later.
        label: Text shown next to the bar.

    Returns:
        The artifact id.
    """
    value = max(0.0, min(100.0, float(percent)))
    return _store("progress", {"value": value, "label": label}, key)


def update_progress(key: str, percent: float, label: str | None = None) -> str:
    """Record a new value for the progress artifact with ``key``.

    Each update is a new artifact row under the same key, so the history is kept.

    Args:
        key: The key given when the progress artifact was created.
        percent: Completion from 0 to 100.
        label: Text shown next to the bar.

    Returns:
        The artifact id.
    """
    return create_progress(percent, key=key, label=label)


def create_link(url: str, text: str | None = None, key: str | None = None) -> str:
    """Attach a link artifact.

    Args:
        url: The link target.
        text: Link text; defaults to the URL.
        key: Optional stable key.

    Returns:
        The artifact id.
    """
    return _store("link", {"url": str(url), "text": text or str(url)}, key)


def create_image(url_or_bytes: str | bytes, key: str | None = None, media_type: str = "image/png") -> str:
    """Attach an image artifact from a URL or raw bytes.

    Args:
        url_or_bytes: An image URL, or the image bytes to embed as a data URI.
        key: Optional stable key.
        media_type: MIME type used when bytes are given.

    Returns:
        The artifact id.
    """
    if isinstance(url_or_bytes, (bytes, bytearray)):
        src = f"data:{media_type};base64,{base64.b64encode(bytes(url_or_bytes)).decode()}"
    else:
        src = str(url_or_bytes)
    return _store("image", {"src": src}, key)
