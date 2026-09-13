# Windows: what works, what does not, what is left

Cereyan builds, installs and runs on Windows, and CI runs every suite there: it compiles
the crates, runs the Rust, Python and documentation suites, builds the x86_64 wheel and
smoke-tests it on every push, with no step skipped by platform. One thing is still open,
and this file is where it is tracked, because the change proposal that describes it lives
under `openspec/`, which is not committed.

It is a pair of **shipped bugs that affect users**, not a test problem. They are marked in
the suite so they cannot be forgotten, and the markers name the workstream that owns them.

## Where it stands

| | Linux | macOS | Windows |
|---|---|---|---|
| Crates compile, clippy, Rust suite | yes | yes | yes |
| Wheel build and smoke test | yes | yes | yes |
| Python suite in CI | yes | yes | yes |
| Documentation suite in CI | yes | yes | yes |
| Served flow `timeout_seconds` | yes | yes | yes |
| Offline flow `timeout_seconds` (`python pipeline.py`, `cereyan run`) | yes | yes | **no** — accepted and ignored |
| Cooperative cancel of a sleeping flow | yes | yes | **no** — waits for the supervisor to terminate the engine |

## Controlling processes in a test

Go through `is_alive`, `terminate`, `kill` and `stop_server` in `tests/server_helpers.py`;
`just lint` fails on a direct `os.kill`, `signal.SIGKILL` or `send_signal` anywhere else in
`tests/`. The rule exists because `os.kill(pid, 0)` **terminates the target on Windows for
any signal value**: the old liveness probe killed the process it asked about, and was why
the Python suite first failed there. The server child is created with
`CREATE_NEW_PROCESS_GROUP` so a control event aimed at it does not reach pytest.

## Interrupting a running flow

Two ways cereyan stops a flow from inside its own process do not work on Windows. The
served timeout is not one of them: the server records `TimedOut` and ends the engine on
every platform, and `test_flow_timeout_kills_engine` passes on Windows.

**An offline flow's timeout is ignored.** `_FlowTimeout` in
`python/cereyan/engine/runner.py` arms only when `signal.setitimer` exists, which it never
does on Windows, so a flow run by `python pipeline.py` or `cereyan run` there runs
unbounded. `docs/guides/retries-timeouts-crashes.md` says so and points readers at
`cereyan serve`. Task-level `timeout_seconds` is unaffected: `ThreadRunner` waits on an
`Event` and `ProcessRunner` polls a pipe, both portable.

**Cooperative cancellation waits for the escalation.** `_CancelWatcher` in
`python/cereyan/engine/child.py` falls back to `_thread.interrupt_main()` where
`signal.pthread_kill` is missing. That only sets the flag CPython checks between bytecodes,
so a flow in `time.sleep` ignores the cancel until the 10 second grace period ends and the
supervisor terminates the engine.

The planned fix, in `windows-interrupt-running-flow`, is `signal.raise_signal(SIGINT)` from
a watcher thread. It goes through CPython's C-level handler, which on Windows also sets the
event `time.sleep` waits on, so it wakes a sleeping main thread with no console involved. A
console control event is not an option: `CTRL_C_EVENT` reaches every process on the
console, the server included. Windows still cannot interrupt most blocking I/O, so a flow
stuck there is timed out when the call returns, or ended by the supervisor when served.

## The markers that pin all of this

Every one is gated on the platform, so none of them fires on Unix. The failures are `xfail`
rather than `skip` so they still run and report XPASS the moment the bug is fixed.

| Location | Kind | Covers |
|---|---|---|
| `tests/test_phase2_offline.py:247` | `xfail(strict)` | offline flow timeout |
| `tests/test_vocabulary.py:228` | `xfail(strict)` | offline flow timeout, in the offline rule test |
| `tests/test_supervisor.py:19` | `xfail(strict)` | cooperative cancel |
| `tests/test_supervisor.py:53` | `skipif` | terminate-then-kill ladder, Unix-only by design |
| `tests/test_cli.py:112` | `skipif` | the user's home cannot be faked by environment |
| `tests/test_release11_socket_routes.py:43` | `skipif` | Unix sockets |
| `tests/test_release11_artifacts_nice.py:75` | `skipif` | engine niceness |

The three `xfail` markers are the ones to watch: when a fix lands, a strict marker turns
XPASS into a failure and forces itself to be removed. Two more tests branch inline rather
than skip: `tests/test_offline.py:97` accepts `PID unknown` for the lock holder, and
`tests/test_phase3_offline.py:108` checks permission bits only on Unix.

A marker used to sit on `test_clock_armed_rule_fires_only_when_window_is_empty`,
blaming Windows for a clock-armed rule that lapsed while its events kept arriving. It was not
a Windows bug. The look-back window of an early tick reached back past the rule's own
creation, where the store is empty because nothing had happened yet, so the rule reported an
absence over time it had not been watching — a coin flip everywhere, decided by where the
first tick fell relative to the first event, and lost more often on Windows only because a
slower start delays that event. It fired on Linux in CI at 1.9.1, is fixed in
`crates/server/src/rules.rs`, and is pinned by
`test_clock_armed_rule_waits_until_it_has_watched_a_whole_window`.

## Differences that are not bugs

These are platform facts and will not change. `docs/design/limitations.md` states the
first two for readers.

- **Unix sockets.** `bind_unix_socket` refuses on other platforms by design; the TCP
  listener and the API token are the way in.
- **Engine niceness.** A negative `priority` lowers engine niceness on Unix only.
  Priority still orders dispatch everywhere — that part is not niceness.
- **The terminate-then-kill ladder.** Windows delivers no `SIGTERM` for a process to
  ignore, and the server's terminate is already `TerminateProcess`, so a run ends in one
  step and never reports `killed`.
- **The lock holder's PID.** Windows reports `PID unknown` under mandatory locking. The
  `local-store` spec already calls the holder's identity best effort; the parts that
  matter hold everywhere, namely that the second opener is refused and no user code runs.
- **The user's home.** `dirs::home_dir()` resolves through the Known Folder API on
  Windows, not `HOME` or `USERPROFILE`, so a test cannot redirect it by environment.
