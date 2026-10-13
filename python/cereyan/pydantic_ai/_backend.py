"""The operation backend: each Pydantic AI durable operation runs as an async unit."""

from __future__ import annotations

from collections.abc import Awaitable, Callable

from pydantic_ai.durable_exec import (
    DurableOperationId,
    JournalCallableOperationBackend,
    RoleBasedOperationConfig,
    ToolsetCallToolId,
)

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

    def __init__(self, *, agent_name: str, default_model_id: str | None) -> None:
        super().__init__(agent_name=agent_name, default_model_id=default_model_id, config=_NO_CONFIG)

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
        if isinstance(operation_id, ToolsetCallToolId):

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
        )
