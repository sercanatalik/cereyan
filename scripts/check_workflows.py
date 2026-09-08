"""Parse every workflow under .github/workflows and check its shape.

A workflow that does not parse is not a failing build: GitHub starts no jobs at
all, the run finishes in seconds with zero jobs, and the result reads like
nothing happened rather than like everything is broken. CI cannot catch this,
because CI is what fails to start, so the check belongs in `just lint`.
"""

import sys
from pathlib import Path

try:
    import yaml
except ImportError:  # pragma: no cover
    sys.exit("check_workflows: needs pyyaml; run through `uv run --with pyyaml`")

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"


def main() -> int:
    problems: list[str] = []
    files = sorted(WORKFLOWS.glob("*.yml")) + sorted(WORKFLOWS.glob("*.yaml"))
    if not files:
        return fail(["no workflow files found under .github/workflows"])
    for path in files:
        rel = path.relative_to(ROOT)
        try:
            doc = yaml.safe_load(path.read_text())
        except yaml.YAMLError as exc:
            problems.append(f"{rel}: does not parse: {exc}")
            continue
        if not isinstance(doc, dict) or not isinstance(doc.get("jobs"), dict):
            problems.append(f"{rel}: has no jobs mapping")
            continue
        names = set(doc["jobs"])
        for name, job in doc["jobs"].items():
            needs = job.get("needs") or []
            for dep in [needs] if isinstance(needs, str) else needs:
                if dep not in names:
                    problems.append(f"{rel}: job {name!r} needs unknown job {dep!r}")
            for i, step in enumerate(job.get("steps") or []):
                if "uses" not in step and "run" not in step:
                    problems.append(f"{rel}: job {name!r} step {i} has neither uses nor run")
    if problems:
        return fail(problems)
    print(f"workflows: {len(files)} parsed, job graph consistent")
    return 0


def fail(problems: list[str]) -> int:
    for p in problems:
        print(f"error: {p}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
