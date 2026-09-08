"""Render the literate files under ``examples/`` to pages under ``docs/examples/``.

An example starts with a header:

    # ---
    # title: Daily ETL
    # description: One sentence.
    # order: 2
    # fixture: offline | served
    # ---

Top-level comment lines (a ``#`` in column one) are prose; everything else is code.
Comment lines inside code (indented) stay in the code block. ``--check`` exits non-zero
when a committed page differs from what would be written.
"""

from __future__ import annotations

import glob
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXAMPLES = os.path.join(ROOT, "examples")
OUT_DIR = os.path.join(ROOT, "docs", "examples")
REPO = "https://github.com/sercanatalik/cereyan/blob/master/examples/"


def parse(path: str) -> tuple[dict[str, str], list[tuple[str, str]]]:
    lines = open(path, encoding="utf-8").read().splitlines()
    if not lines or lines[0].strip() != "# ---":
        raise SystemExit(f"{path}: literate examples start with a '# ---' header")
    meta: dict[str, str] = {}
    i = 1
    while i < len(lines) and lines[i].strip() != "# ---":
        key, _, value = lines[i].lstrip("# ").partition(":")
        meta[key.strip()] = value.strip()
        i += 1
    if i >= len(lines):
        raise SystemExit(f"{path}: header is not closed with '# ---'")
    for key in ("title", "description", "order"):
        if key not in meta:
            raise SystemExit(f"{path}: header lacks {key}")
    int(meta["order"])
    if meta.get("fixture", "offline") not in ("offline", "served"):
        raise SystemExit(f"{path}: fixture must be offline or served")

    segments: list[tuple[str, str]] = []
    kind = None
    buf: list[str] = []

    def flush() -> None:
        nonlocal buf
        text = "\n".join(buf).strip("\n")
        if text.strip():
            segments.append((kind, text))
        buf = []

    for line in lines[i + 1 :]:
        is_prose = line.startswith("#") and (len(line) == 1 or line[1] == " ")
        if line.strip() == "":
            buf.append(line)
            continue
        new_kind = "prose" if is_prose else "code"
        if new_kind != kind:
            flush()
            kind = new_kind
        buf.append(line[2:] if is_prose else line)
    flush()
    return meta, segments


def render(path: str) -> str:
    meta, segments = parse(path)
    base = os.path.basename(path)
    served = meta.get("fixture", "offline") == "served"
    out = [f"# {meta['title']}", "", meta["description"], ""]
    run_line = f"Source: [`examples/{base}`]({REPO}{base}). Run it with `python examples/{base}`"
    run_line += " while `cereyan serve examples/` is running." if served else " (no server needed)."
    out += [run_line, ""]
    for kind, text in segments:
        if kind == "prose":
            out.append(text)
        else:
            out.append("```python")
            out.append(text)
            out.append("```")
        out.append("")
    return "\n".join(out).rstrip() + "\n"


def main(argv: list[str]) -> int:
    check = "--check" in argv
    stale = []
    files = sorted(glob.glob(os.path.join(EXAMPLES, "*.py")))
    for path in files:
        text = render(path)
        out = os.path.join(OUT_DIR, os.path.splitext(os.path.basename(path))[0] + ".md")
        if check:
            current = open(out, encoding="utf-8").read() if os.path.exists(out) else None
            if current != text:
                stale.append(os.path.relpath(out, ROOT))
        else:
            os.makedirs(OUT_DIR, exist_ok=True)
            with open(out, "w", encoding="utf-8") as fh:
                fh.write(text)
    if check:
        for s in stale:
            print(f"stale: {s} (run `just docs`)")
        return 1 if stale else 0
    print(f"wrote {len(files)} example page(s) to docs/examples/")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
