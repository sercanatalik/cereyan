"""Durable Pydantic AI agents: every model request and tool call a replayable task run.

Install with ``pip install "cereyan[pydantic-ai]"``; nothing else in cereyan imports this
module. See the guide "Make a Pydantic AI agent durable".
"""

from __future__ import annotations

try:
    import pydantic_ai.durable_exec  # noqa: F401
except ImportError as _import_error:  # pragma: no cover - exercised without the extra
    raise ImportError(
        'cereyan.pydantic_ai needs Pydantic AI: pip install "cereyan[pydantic-ai]"'
    ) from _import_error

from ._capability import CereyanDurability, CereyanParallelExecutionMode
from ._settle import settle

__all__ = ["CereyanDurability", "CereyanParallelExecutionMode", "settle"]
