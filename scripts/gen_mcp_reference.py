"""Generate ``docs/reference/mcp.md`` from ``tests/mcp_snapshot.json`` and the page template.

The snapshot is written by ``tests/test_agent_mcp.py`` with ``CEREYAN_UPDATE_SNAPSHOTS=1``
and holds what a client receives: the ``initialize`` result, ``tools/list``,
``resources/templates/list``, ``prompts/list``, and the top-level response keys of every
tool. ``--check`` exits non-zero when the committed page differs from what would be written.
"""

from __future__ import annotations

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SNAPSHOT = os.path.join(ROOT, "tests", "mcp_snapshot.json")
TEMPLATE = os.path.join(ROOT, "scripts", "mcp_reference_template.md")
OUT = os.path.join(ROOT, "docs", "reference", "mcp.md")

READ_ONLY = "Read-only."


def _fail(message: str) -> None:
    print(f"gen_mcp_reference: {message}")
    raise SystemExit(1)


def _argument(name: str, schema: dict, required: bool) -> str:
    """One argument as `name` followed by its type, requiredness, range, and default."""
    facts = []
    kind = schema.get("type")
    if kind == "array":
        item = schema.get("items", {}).get("type", "any")
        facts.append(f"array of {item}")
    elif kind:
        facts.append(kind)
    else:
        facts.append("any JSON")
    low, high = schema.get("minimum"), schema.get("maximum")
    if low is not None and high is not None:
        facts.append(f"{low} to {high}")
    if required:
        facts.append("required")
    elif "default" in schema:
        facts.append(f"default `{json.dumps(schema['default'])}`")
    described = f"`{name}` ({', '.join(facts)})"
    description = schema.get("description")
    return f"{described} — {description}" if description else described


def _arguments(schema: dict) -> str:
    props = schema.get("properties") or {}
    if not props:
        return "none"
    required = set(schema.get("required") or [])
    return "<br>".join(_argument(n, s, n in required) for n, s in props.items())


def _responses(entry: dict | None) -> str:
    if not entry:
        return "—"
    parts = []
    for label, shape in entry.items():
        keys = ", ".join(f"`{k}`" for k in shape["keys"])
        parts.append(keys if label == "default" else f"{label}: {keys}")
    return "<br>".join(parts)


def _tool_table(tools: list[dict], response_keys: dict, out: list[str]) -> None:
    out.append("| Tool | Arguments | Returns |")
    out.append("|---|---|---|")
    for tool in tools:
        description = tool["description"].removesuffix(READ_ONLY).strip()
        out.append(
            f"| `{tool['name']}` | {_arguments(tool['inputSchema'])} | {description} "
            f"Response keys: {_responses(response_keys.get(tool['name']))} |"
        )
    out.append("")


def _item_table(doc: dict) -> list[str]:
    """Every response key that holds a list, with the fields of one item."""
    rows = []
    for name, variants in doc["response_keys"].items():
        for label, shape in variants.items():
            for key, fields in (shape.get("items") or {}).items():
                where = f"`{name}`" if label == "default" else f"`{name}` ({label})"
                rows.append(f"| {where} | `{key}` | {', '.join(f'`{f}`' for f in fields)} |")
    if not rows:
        return []
    return [
        "### Fields of the lists a tool returns",
        "",
        "Recorded from real responses, so a model knows what it gets without a second call.",
        "",
        "| Tool | Key | Item fields |",
        "|---|---|---|",
        *sorted(set(rows)),
        "",
    ]


def _handshake(doc: dict) -> list[str]:
    hs = doc["handshake"]
    capabilities = ", ".join(f"`{c}`" for c in sorted(hs["capabilities"]))
    return [
        f"`initialize` answers with protocol version `{hs['protocolVersion']}`, server name "
        f"`{hs['serverInfo']['name']}` and its release version, and the capabilities {capabilities}. "
        "It also carries the instructions the server gives the model:",
        "",
        "> " + hs["instructions"],
        "",
    ]


def _tools(doc: dict) -> list[str]:
    read_only, changing = [], []
    for tool in doc["tools"]:
        if tool["description"].endswith(READ_ONLY):
            read_only.append(tool)
        elif tool["description"].endswith("."):
            changing.append(tool)
        else:
            _fail(
                f"tool {tool['name']!r} has a description that neither ends in {READ_ONLY!r} nor in a "
                "full stop, so it cannot be classified; fix the description in crates/server/src/mcp.rs"
            )
    keys = doc["response_keys"]
    out = [f"### Read-only tools ({len(read_only)})", ""]
    _tool_table(read_only, keys, out)
    out += [f"### Tools that change state ({len(changing)})", ""]
    _tool_table(changing, keys, out)
    return out + _item_table(doc)


def _resources(doc: dict) -> list[str]:
    out = [
        "`resources/list` returns none: both resources are templates, listed by "
        "`resources/templates/list` and fetched with `resources/read`."
        if not doc["resources"]
        else "`resources/list` returns the resources below.",
        "",
        "| URI template | Name | Content | MIME type |",
        "|---|---|---|---|",
    ]
    for r in doc["resource_templates"]:
        out.append(f"| `{r['uriTemplate']}` | {r['name']} | {r['description']} | `{r['mimeType']}` |")
    out.append("")
    return out


def _prompts(doc: dict) -> list[str]:
    out = ["| Prompt | Arguments | Purpose |", "|---|---|---|"]
    for p in doc["prompts"]:
        args = (
            "<br>".join(
                f"`{a['name']}` ({a['description']}{', required' if a.get('required') else ''})"
                for a in p.get("arguments") or []
            )
            or "none"
        )
        out.append(f"| `{p['name']}` | {args} | {p['description']} |")
    out.append("")
    for name, messages in (doc.get("prompt_messages") or {}).items():
        out.append(f"`{name}` renders one message:")
        out.append("")
        for message in messages:
            out.append(f"> **{message['role']}**: {message['content']['text']}")
            out.append("")
    return out


SECTIONS = {"handshake": _handshake, "tools": _tools, "resources": _resources, "prompts": _prompts}


def render() -> str:
    doc = json.load(open(SNAPSHOT, encoding="utf-8"))
    template = open(TEMPLATE, encoding="utf-8").read().splitlines()
    header = "<!-- Generated by scripts/gen_mcp_reference.py from tests/mcp_snapshot.json. Do not edit; run `just docs`. -->"
    out: list[str] = []
    seen = set()
    for line in template:
        marker = line.strip()
        if marker.startswith("<!-- generated:") and marker.endswith("-->"):
            name = marker[len("<!-- generated:") : -len("-->")].strip()
            if name not in SECTIONS:
                _fail(f"unknown marker {name!r} in {os.path.relpath(TEMPLATE, ROOT)}")
            seen.add(name)
            section = SECTIONS[name](doc)
            while section and not section[-1]:
                section.pop()
            out += section
            continue
        out.append(line)
        if line.startswith("# "):
            out += ["", header]
    missing = sorted(set(SECTIONS) - seen)
    if missing:
        _fail(f"template is missing the markers: {', '.join(missing)}")
    return "\n".join(out).rstrip() + "\n"


def main(argv: list[str]) -> int:
    text = render()
    if "--check" in argv:
        current = open(OUT, encoding="utf-8").read() if os.path.exists(OUT) else None
        if current != text:
            print(f"stale: {os.path.relpath(OUT, ROOT)} (run `just docs`)")
            return 1
        return 0
    with open(OUT, "w", encoding="utf-8") as fh:
        fh.write(text)
    print(f"wrote {os.path.relpath(OUT, ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
