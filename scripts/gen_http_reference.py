"""Generate ``docs/reference/http-api.md`` from ``ui/openapi.snapshot.json``.

``--check`` exits non-zero when the committed page differs from what would be written.
"""

from __future__ import annotations

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SNAPSHOT = os.path.join(ROOT, "ui", "openapi.snapshot.json")
OUT = os.path.join(ROOT, "docs", "reference", "http-api.md")

METHOD_ORDER = ["get", "post", "put", "patch", "delete"]
TOKEN_EXEMPT = {"/api/health"}
ENGINE_PREFIX = "/api/engine/"
GROUPS = [
    ("Health and server", ["/api/health", "/api/server", "/api/settings", "/api/counts", "/api/stream", "/api/vocabulary"]),
    ("Flows", ["/api/flows"]),
    ("Runs", ["/api/runs"]),
    ("Task runs", ["/api/task-runs"]),
    ("Schedules", ["/api/schedules"]),
    ("Backfills", ["/api/backfills"]),
    ("Events", ["/api/events"]),
    ("Rules", ["/api/rules"]),
    ("Artifacts", ["/api/artifacts"]),
    ("Variables", ["/api/variables"]),
    ("Resources", ["/api/resources"]),
    ("Engine (internal)", ["/api/engine"]),
]


def _ref_name(schema: dict | None) -> str | None:
    if not schema:
        return None
    if "$ref" in schema:
        return schema["$ref"].rsplit("/", 1)[-1]
    if schema.get("type") == "array" and "$ref" in schema.get("items", {}):
        return schema["items"]["$ref"].rsplit("/", 1)[-1] + "[]"
    return None


def _type(schema: dict | None) -> str:
    if not schema:
        return ""
    ref = _ref_name(schema)
    if ref:
        return f"[`{ref.rstrip('[]')}`](#{ref.rstrip('[]').lower()}){'[]' if ref.endswith('[]') else ''}"
    if "allOf" in schema and len(schema["allOf"]) == 1:
        return _type(schema["allOf"][0])
    if "oneOf" in schema:
        return " or ".join(_type(s) for s in schema["oneOf"])
    t = schema.get("type")
    if isinstance(t, list):
        t = " or ".join(x for x in t if x != "null") + (" or null" if "null" in t else "")
    if t == "array":
        return f"array of {_type(schema.get('items')) or 'any'}"
    if "enum" in schema:
        return " or ".join(f"`{v}`" for v in schema["enum"])
    fmt = schema.get("format")
    return f"{t or 'any'}{f' ({fmt})' if fmt else ''}"


def _group_of(path: str) -> str:
    for name, prefixes in GROUPS:
        if any(path == p or path.startswith(p + "/") or path.startswith(p + "?") for p in prefixes):
            return name
    return "Other"


def _operation(path: str, method: str, op: dict, out: list[str]) -> None:
    out.append(f"### `{method.upper()} {path}`")
    out.append("")
    notes = []
    if path in TOKEN_EXEMPT:
        notes.append("Exempt from the API token.")
    if path.startswith(ENGINE_PREFIX):
        notes.append("Used by engine processes to report to the server; not intended for clients and not covered by compatibility promises.")
    summary = op.get("summary") or op.get("description") or ""
    if summary:
        out.append(summary.strip())
        out.append("")
    if notes:
        out.append("!!! note")
        for n in notes:
            out.append(f"    {n}")
        out.append("")
    params = op.get("parameters", [])
    if params:
        out.append("| Parameter | In | Type | Required | Description |")
        out.append("|---|---|---|---|---|")
        for p in params:
            desc = (p.get("description") or "").replace("\n", " ").replace("|", "\\|")
            out.append(f"| `{p['name']}` | {p['in']} | {_type(p.get('schema'))} | {'yes' if p.get('required') else 'no'} | {desc} |")
        out.append("")
    body = op.get("requestBody")
    if body:
        content = body.get("content", {})
        for media, spec in content.items():
            out.append(f"**Request body** ({media}{'' if body.get('required') else ', optional'}): {_type(spec.get('schema')) or 'any'}")
            out.append("")
    responses = op.get("responses", {})
    if responses:
        out.append("| Status | Body |")
        out.append("|---|---|")
        for status in sorted(responses):
            r = responses[status]
            content = r.get("content", {})
            body_type = ", ".join(f"{_type(spec.get('schema')) or 'any'} ({media})" for media, spec in content.items()) or (r.get("description") or "no body")
            out.append(f"| {status} | {body_type} |")
        out.append("")


def _schema(name: str, schema: dict, out: list[str]) -> None:
    out.append(f"### `{name}`")
    out.append("")
    if schema.get("description"):
        out.append(schema["description"].strip())
        out.append("")
    if "enum" in schema:
        out.append("One of: " + ", ".join(f"`{v}`" for v in schema["enum"]) + ".")
        out.append("")
        return
    if "oneOf" in schema:
        out.append("One of: " + ", ".join(_type(s) for s in schema["oneOf"]) + ".")
        out.append("")
        return
    props = schema.get("properties")
    if not props:
        out.append(f"Type: {_type(schema)}.")
        out.append("")
        return
    required = set(schema.get("required", []))
    out.append("| Field | Type | Required | Description |")
    out.append("|---|---|---|---|")
    for field in sorted(props):
        desc = (props[field].get("description") or "").replace("\n", " ").replace("|", "\\|")
        out.append(f"| `{field}` | {_type(props[field])} | {'yes' if field in required else 'no'} | {desc} |")
    out.append("")


def render() -> str:
    doc = json.load(open(SNAPSHOT, encoding="utf-8"))
    paths = doc["paths"]
    out = [
        "# HTTP API",
        "",
        "<!-- Generated by scripts/gen_http_reference.py from ui/openapi.snapshot.json. Do not edit; run `just docs`. -->",
        "",
        f"The server's OpenAPI document (version {doc['info']['version']}) is served at `GET /api/openapi.json`; this page is rendered from the checked-in snapshot that the UI client and the test suite are generated from. Every `/api/*` route except `/api/health` requires `Authorization: Bearer <token>` when a token is set (see [Secure the server](../guides/secure-the-server.md)); requests over the Unix socket skip the check. Responses are JSON; errors carry an `ErrorBody`. Times are microseconds since the Unix epoch in UTC. List endpoints page by keyset cursor.",
        "",
        "The MCP endpoint (`POST /mcp`) is not part of the OpenAPI document; see [MCP tools, resources and prompts](mcp.md).",
        "",
    ]
    grouped: dict[str, list[str]] = {}
    for path in paths:
        grouped.setdefault(_group_of(path), []).append(path)
    for name, _ in GROUPS + [("Other", [])]:
        if name not in grouped:
            continue
        out.append(f"## {name}")
        out.append("")
        for path in sorted(grouped[name]):
            for method in METHOD_ORDER:
                if method in paths[path]:
                    _operation(path, method, paths[path][method], out)
    out.append("## Schemas")
    out.append("")
    for name in sorted(doc["components"]["schemas"]):
        _schema(name, doc["components"]["schemas"][name], out)
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
