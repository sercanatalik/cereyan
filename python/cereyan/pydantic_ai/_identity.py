"""What a replayed unit must match: the question an operation asks, not when it asked.

Pydantic AI hands a backend a ``cache_key`` of raw objects. A model request's messages
carry timestamps and run ids made fresh on every pass, so hashing them as they are
would never match a checkpoint. These projections keep the content and drop the rest.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any

from pydantic_ai.durable_exec import ModelRequestId, ToolsetCallToolId
from pydantic_ai.messages import ModelMessagesTypeAdapter

# Keys whose values are made afresh on every pass, at any depth of a dumped message.
VOLATILE_KEYS = frozenset({"timestamp", "run_id", "conversation_id", "provider_response_id"})
# Prefix of the ids Pydantic AI generates for calls a provider left unnamed.
GENERATED_ID_PREFIX = "pyd_ai_"


def _digest(value: Any) -> str:
    text = json.dumps(value, sort_keys=True, separators=(",", ":"), default=repr)
    return hashlib.sha256(text.encode()).hexdigest()


def _strip(value: Any, generated: dict[str, str]) -> Any:
    """Drop volatile keys and number generated ids by first appearance."""
    if isinstance(value, dict):
        return {k: _strip(v, generated) for k, v in value.items() if k not in VOLATILE_KEYS}
    if isinstance(value, list):
        return [_strip(v, generated) for v in value]
    if isinstance(value, str) and value.startswith(GENERATED_ID_PREFIX):
        return generated.setdefault(value, f"{GENERATED_ID_PREFIX}{len(generated)}")
    return value


def _jsonable(value: Any) -> Any:
    try:
        from pydantic_core import to_jsonable_python

        return to_jsonable_python(value, fallback=repr)
    except Exception:  # noqa: BLE001 - an identity must never fail the operation
        return repr(value)


def model_request_identity(cache_key: tuple[object, ...]) -> Any:
    """The messages and settings of a model request, without what changes between passes."""
    _model_id, messages, settings, *_ = cache_key
    try:
        dumped = ModelMessagesTypeAdapter.dump_python(messages, mode="json")
    except Exception:  # noqa: BLE001
        dumped = _jsonable(messages)
    return {"messages": _strip(dumped, {}), "settings": _jsonable(settings)}


def tool_call_identity(cache_key: tuple[object, ...]) -> Any:
    """A tool call: its name, its arguments, and the id the model gave the call."""
    name, args, ctx, *_ = cache_key
    return {"tool": name, "args": _jsonable(args), "tool_call_id": getattr(ctx, "tool_call_id", None)}


def input_hash(operation_id: object, name: str, cache_key: tuple[object, ...]) -> str:
    """The input hash a unit's checkpoint is matched on."""
    if isinstance(operation_id, ModelRequestId):
        return _digest(["model", model_request_identity(cache_key)])
    if isinstance(operation_id, ToolsetCallToolId):
        return _digest(["tool", tool_call_identity(cache_key)])
    # Tool discovery, event handlers, capability operations, and any operation a newer
    # Pydantic AI adds: the name and its position in the run identify them.
    return _digest(["name", name])
