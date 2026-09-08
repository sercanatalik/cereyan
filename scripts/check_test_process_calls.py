"""Keep raw process control out of the tests.

On Windows `os.kill` understands only CTRL_C_EVENT and CTRL_BREAK_EVENT; every
other value, zero included, terminates the target. So `os.kill(pid, 0)` — the
ordinary Unix liveness probe — kills the process it is asked about there, and a
test using it does not fail, it quietly changes the answer. `send_signal(SIGTERM)`
is the same shape of trap: a hard kill on Windows, so a server stopped that way
never runs its shutdown path.

`tests/server_helpers.py` carries both differences. Everything else goes through
`is_alive`, `terminate`, `kill` and `stop_server`.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TESTS = ROOT / "tests"
ALLOWED = {"server_helpers.py"}
BANNED = [
    (re.compile(r"\bos\.kill\s*\("), "os.kill", "is_alive(pid) / terminate(pid) / kill(pid)"),
    (re.compile(r"\bsignal\.SIGKILL\b"), "signal.SIGKILL", "kill(pid)"),
    (re.compile(r"\.send_signal\s*\("), "send_signal", "stop_server(proc)"),
]


def main() -> int:
    problems = []
    for path in sorted(TESTS.rglob("*.py")):
        if path.name in ALLOWED:
            continue
        for lineno, line in enumerate(path.read_text().splitlines(), 1):
            if line.lstrip().startswith("#"):
                continue
            for pattern, name, use in BANNED:
                if pattern.search(line):
                    rel = path.relative_to(ROOT)
                    problems.append(f"{rel}:{lineno}: {name} — use {use} from server_helpers")
    if problems:
        for p in problems:
            print(f"error: {p}", file=sys.stderr)
        print(
            "\nProcess control differs between platforms; server_helpers carries the difference.",
            file=sys.stderr,
        )
        return 1
    print("tests: no raw process control outside server_helpers")
    return 0


if __name__ == "__main__":
    sys.exit(main())
