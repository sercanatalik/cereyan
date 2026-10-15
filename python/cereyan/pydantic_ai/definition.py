"""The shape of an agent definition: what the UI stores and the runner builds from.

The server validates a definition when it is saved (its serde struct mirrors
this); this module is the runner's and the catalogue's view of the same JSON.
"""

from __future__ import annotations

import re
from typing import Any

TOOL_KINDS = ("capability", "common", "mcp", "self", "code")
APPROVAL_POLICIES = ("ask", "approve", "deny")
OUTPUT_KINDS = ("text", "json")

#: The module every UI agent's flow is registered on.
RUNNER_MODULE = "cereyan.pydantic_ai.runner"

# `{name}`, not `{{name}}`: a brace variable of a prompt or instructions template.
_VARIABLE = re.compile(r"(?<!\{)\{([A-Za-z_][A-Za-z0-9_]*)\}(?!\})")


def template_variables(*texts: str) -> list[str]:
    """The brace variables of the texts, in first-seen order, without duplicates."""
    seen: list[str] = []
    for text in texts:
        for name in _VARIABLE.findall(text or ""):
            if name not in seen:
                seen.append(name)
    return seen


def parameter_schema(definition: dict[str, Any]) -> dict[str, Any]:
    """The flow's parameter schema: ``prompt`` with the template as its default,
    and every other template variable as an optional string."""
    properties: dict[str, Any] = {"prompt": {"type": "string", "default": definition.get("prompt", "")}}
    for name in template_variables(definition.get("prompt", ""), definition.get("instructions", "")):
        if name != "prompt":
            properties[name] = {"type": "string", "default": ""}
    return {"type": "object", "properties": properties}


def render(template: str, values: dict[str, Any]) -> str:
    """Fill ``{name}`` holes from ``values``; an unknown hole stays as written."""

    def fill(match: re.Match) -> str:
        name = match.group(1)
        return str(values[name]) if name in values else match.group(0)

    return _VARIABLE.sub(fill, template or "")


def validate(definition: dict[str, Any]) -> dict[str, Any]:
    """Check a definition's shape and fill its defaults; raises ``ValueError`` naming the field."""
    if not isinstance(definition, dict):
        raise ValueError("definition: must be an object")
    out = dict(definition)
    for field in ("name", "model"):
        value = out.get(field)
        if not isinstance(value, str) or not value.strip():
            raise ValueError(f"{field}: required")
    out["project"] = str(out.get("project") or "agents")
    out["group"] = out.get("group") or None
    out["description"] = str(out.get("description") or "")
    out["instructions"] = str(out.get("instructions") or "")
    out["prompt"] = str(out.get("prompt") or "")
    key = out.get("api_key")
    if key is not None and not isinstance(key, str):
        raise ValueError("api_key: must be a variable name or null")
    tools = out.get("tools") or []
    if not isinstance(tools, list):
        raise ValueError("tools: must be a list")
    for i, tool in enumerate(tools):
        kind = (tool or {}).get("kind")
        if kind not in TOOL_KINDS:
            raise ValueError(f"tools[{i}].kind: must be one of {', '.join(TOOL_KINDS)}")
        if kind == "code":
            raise ValueError(f"tools[{i}].kind: code tools are not available yet")
        if not isinstance(tool.get("name"), str) or not tool["name"]:
            raise ValueError(f"tools[{i}].name: required")
        tool.setdefault("options", {})
    out["tools"] = tools
    policy = dict(out.get("policy") or {})
    policy.setdefault("approvals", "ask")
    if policy["approvals"] not in APPROVAL_POLICIES:
        raise ValueError("policy.approvals: must be ask, approve or deny")
    policy["once"] = [str(t) for t in (policy.get("once") or [])]
    out["policy"] = policy
    limits = dict(out.get("limits") or {})
    for name in ("tokens", "requests", "cost_usd", "timeout_seconds"):
        value = limits.get(name)
        if value is not None and (not isinstance(value, (int, float)) or value < 0):
            raise ValueError(f"limits.{name}: must be a non-negative number")
    out["limits"] = limits
    output = dict(out.get("output") or {"kind": "text"})
    if output.get("kind") not in OUTPUT_KINDS:
        raise ValueError("output.kind: must be text or json")
    if output["kind"] == "json" and not isinstance(output.get("schema"), dict):
        raise ValueError("output.schema: required for json output")
    out["output"] = output
    out["crash_retries"] = int(out.get("crash_retries") or 0)
    out["enabled"] = bool(out.get("enabled", True))
    return out
