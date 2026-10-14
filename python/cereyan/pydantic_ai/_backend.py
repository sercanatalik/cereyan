"""The operation backend: each Pydantic AI durable operation runs as an async unit."""

from __future__ import annotations

import inspect
from collections.abc import Awaitable, Callable, Mapping

from pydantic_ai.durable_exec import (
    DurableOperationId,
    JournalCallableOperationBackend,
    RoleBasedOperationConfig,
    ToolsetCallToolId,
)

from .. import context
from ..effects import NOT_DONE
from ..engine.runner import run_async_unit
from ._identity import input_hash

# Every kind a tool's encoded `CallToolResult` can take. Control flow is recorded like a
# return value: a gap would end replay and re-run every model request after it.
KNOWN_TOOL_RESULT_KINDS = frozenset(
    {
        "tool_return",
        "tool_content_result",
        "approval_required",
        "call_deferred",
        "model_retry",
        "validation_error",
        "tool_failed",
    }
)

_NO_CONFIG = RoleBasedOperationConfig[None](model=None, event=None, capability=None, tool=None)


class CereyanOperationBackend(JournalCallableOperationBackend[None]):
    """Runs each operation as one task run of the current cereyan run."""

    def __init__(self, *, agent_name: str, default_model_id: str | None,
                 once_tools: Mapping[str, Callable | None] | None = None) -> None:
        super().__init__(agent_name=agent_name, default_model_id=default_model_id, config=_NO_CONFIG)
        self.once_tools = dict(once_tools or {})

    async def execute(
        self,
        *,
        operation_id: DurableOperationId,
        name: str,
        body: Callable[[], Awaitable[object]],
        cache_key: tuple[object, ...],
        config: None,
    ) -> object:
        del config
        checked = body
        once: dict = {}
        if isinstance(operation_id, ToolsetCallToolId):
            tool_name, args, *_ = cache_key
            if tool_name in self.once_tools:
                once = {"once": True, "probe": _probe(self.once_tools[tool_name], dict(args or {})),
                        "defer": _defer(str(tool_name))}

            async def checked() -> object:
                payload = await body()
                kind = payload.get("kind") if isinstance(payload, dict) else None
                if kind not in KNOWN_TOOL_RESULT_KINDS:
                    raise RuntimeError(f"tool result of unknown kind {kind!r}: refusing to record it")
                return payload

        return await run_async_unit(
            name,
            f"pydantic_ai:{name}",
            checked,
            input_hash=input_hash(operation_id, name, cache_key),
            serializer="json",
            **once,
        )


def _probe(probe: Callable | None, args: dict):
    """The unit's probe for a once tool: the user's probe over the tool's arguments,
    its value encoded as the tool's return."""
    if probe is None:
        return None

    async def run() -> object:
        found = probe(**args)
        if inspect.isawaitable(found):
            found = await found
        return found if found is NOT_DONE else {"kind": "tool_return", "result": found}

    return run


def _defer(tool_name: str):
    """The unit's result for an in-doubt once tool nothing settled: an approval request
    the flow answers, marked so `settle` recognises it."""

    def build() -> dict:
        task = context.current_task_run()
        mark = {"in_doubt": True, "tool": tool_name, "effect_key": task.effect_key if task else None}
        return {"kind": "approval_required", "metadata": {"cereyan": mark}}

    return build
