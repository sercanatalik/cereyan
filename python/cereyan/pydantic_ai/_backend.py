"""The operation backend: each Pydantic AI durable operation runs as an async unit."""

from __future__ import annotations

import inspect
import json
from collections.abc import Awaitable, Callable, Mapping

from pydantic_ai.durable_exec import (
    DurableOperationId,
    JournalCallableOperationBackend,
    ModelRequestId,
    RoleBasedOperationConfig,
    ToolsetCallToolId,
)

from .. import masking

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
        self.agent_name = agent_name
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
            summarize=lambda result: summarize(self.agent_name, operation_id, cache_key, result),
            **once,
        )


#: How much of a text or a result a summary keeps.
PREVIEW = 500


def _preview(value: object) -> str:
    text = value if isinstance(value, str) else json.dumps(value, default=str)
    text = masking.mask(text) if masking.active() else text
    return text if len(text) <= PREVIEW else text[:PREVIEW] + "…"


def _jsonable(value: object) -> object:
    try:
        from pydantic_core import to_jsonable_python

        return to_jsonable_python(value, fallback=repr)
    except Exception:  # noqa: BLE001
        return repr(value)


def summarize(agent_name: str, operation_id: DurableOperationId, cache_key: tuple[object, ...], result: object) -> dict | None:
    """What a unit did, in a shape the UI can show: the same on every Pydantic AI version."""
    if isinstance(operation_id, ModelRequestId):
        model_id = cache_key[0] if cache_key else None
        payload = result if isinstance(result, dict) else _jsonable(result)
        parts = payload.get("parts", []) if isinstance(payload, dict) else []
        text = "\n".join(str(p.get("content", "")) for p in parts if isinstance(p, dict) and p.get("part_kind") == "text")
        calls = [{"tool": p.get("tool_name"), "args": _jsonable(p.get("args"))}
                 for p in parts if isinstance(p, dict) and p.get("part_kind") == "tool-call"]
        usage = payload.get("usage", {}) if isinstance(payload, dict) else {}
        return {
            "kind": "model",
            "agent": agent_name,
            "model": str(model_id) if model_id is not None else payload.get("model_name") if isinstance(payload, dict) else None,
            "usage": {"input": int(usage.get("input_tokens") or 0), "output": int(usage.get("output_tokens") or 0)},
            "text": _preview(text),
            "tool_calls": [{"tool": c["tool"], "args": json.loads(_preview(c["args"])) if isinstance(c["args"], dict) and len(json.dumps(c["args"], default=str)) <= PREVIEW else _preview(c["args"])} for c in calls],
        }
    if isinstance(operation_id, ToolsetCallToolId):
        tool_name, args, *_ = cache_key
        payload = result if isinstance(result, dict) else {}
        return {
            "kind": "tool",
            "agent": agent_name,
            "tool": str(tool_name),
            "args": _jsonable(args) if len(json.dumps(_jsonable(args), default=str)) <= PREVIEW else _preview(_jsonable(args)),
            "result": _preview(payload.get("result", payload)),
            "result_kind": payload.get("kind"),
        }
    return None


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
