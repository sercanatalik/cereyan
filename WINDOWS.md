# Windows: what works, what does not, what is left

Cereyan builds, installs and runs on Windows: CI compiles the crates, runs the Rust
suite, builds the x86_64 wheel and smoke-tests it on every push. Two things are
still open, and this file is where they are tracked, because the change proposals
that describe them live under `openspec/`, which is not committed.

One of the two is a pair of **shipped bugs that affect users**, not test problems. They
are marked in the suite so they cannot be forgotten, and the markers name the workstream
that owns each.

## Where it stands

| | Linux | macOS | Windows |
|---|---|---|---|
| Crates compile, clippy, Rust suite | yes | yes | yes |
| Wheel build and smoke test | yes | yes | yes |
| Python suite in CI | yes | yes | **no** — `ci.yml` line 70, `if: runner.os != 'Windows'` |
| Documentation suite in CI | yes | yes | **no** — same condition |
| Flow `timeout_seconds` | yes | yes | **no** — accepted and silently ignored |
| Cooperative cancel of a blocked flow | yes | yes | **no** — waits for the supervisor to terminate the engine |

## 1. Turn the Python and documentation suites back on

The suites were switched off for Windows after the first run there, and the harness
has since been fixed: `tests/server_helpers.py` gained `is_alive`, `terminate`, `kill`
and `stop_server`, because the old liveness probe used `os.kill(pid, 0)`, which
**terminates the target on Windows for any signal value** — the probe was killing the
process it asked about. The server child is now created with `CREATE_NEW_PROCESS_GROUP`
so a control event aimed at it does not reach pytest.

Two product bugs were found and fixed along the way: `serve.py` now handles `SIGBREAK`
as well as `SIGTERM`, without which a console control event ended the process before
`server.stop()` and `close_store()` could run; and the default-home test was writing
into the runner's real `~/.cereyan`, because `dirs::home_dir()` reads `USERPROFILE` on
Windows and the test only set `HOME`.

Still to do, all of it needing a Windows machine or a Windows CI run:

- Get the offline tests passing — they exercise the store and the decorators without
  starting a server, so they isolate everything that is not process control.
- Get `tests/test_server_api.py`, `tests/test_supervisor.py` and
  `tests/test_engine_shutdown.py` passing, or skipping with a stated reason.
- Work through the rest of `tests/` in whatever order CI reveals.
- Run the documentation suite and fix or skip what breaks.
- Remove the `if: runner.os != 'Windows'` conditions from `.github/workflows/ci.yml`
  and the gap note from the `just test` row in `docs/contributing.md`, then confirm CI
  is green on all three platforms with no platform-conditional steps left.

Do not remove those conditions before the suites pass: turning Windows on early makes
CI red rather than making it honest.

## 2. Interrupting a running flow

Cereyan interrupts a running flow in two situations, and both depend on a Unix signal
reaching the main thread. Neither works on Windows, and neither says so.

**The flow timeout is a silent no-op.** `_FlowTimeout` in `python/cereyan/engine/runner.py`
arms only when `hasattr(signal, "setitimer")`, which is never true on Windows, so
`self.active` stays `False` and the flow runs unbounded: no warning, no error, no
`TimedOut`. `docs/guides/retries-timeouts-crashes.md` states that a flow exceeding its
timeout ends `TimedOut`, with no platform caveat, so a Windows reader is told something
untrue. Task-level `timeout_seconds` is unaffected: `ThreadRunner` waits on an `Event`
and `ProcessRunner` polls a pipe, both portable.

**Cooperative cancellation is not cooperative.** `_CancelWatcher` in
`python/cereyan/engine/child.py` calls `signal.pthread_kill(main, SIGINT)`, which does
not exist on Windows; the fallback `_thread.interrupt_main()` only takes effect between
bytecodes and cannot interrupt a blocking call. A flow sleeping or blocked on I/O
ignores the cancel and runs until the supervisor terminates the engine.

These are one problem — there is no portable way to interrupt a blocking call in the
main thread, and both features assumed there was — so the mechanism should be decided
once. A watchdog thread is portable but cannot interrupt a blocking C call the way
`SIGALRM` can, so it is not equivalent; ending the engine is always available and always
abrupt. Whichever is chosen, accepting `timeout_seconds` and ignoring it has to stop.

## The markers that pin all of this

Every one is gated on `sys.platform == "win32"`, so none of them fires on Unix. The failures are `xfail` rather than `skip` so they
still run and report XPASS the moment the bug is fixed.

| Location | Kind | Covers |
|---|---|---|
| `tests/test_phase2_offline.py:247` | `xfail(strict)` | flow timeout, section 2 |
| `tests/test_supervisor.py:19` | `xfail(strict)` | cooperative cancel, section 2 |
| `tests/test_supervisor.py:53` | `skipif` | terminate-then-kill ladder, Unix-only by design |
| `tests/test_cli.py:112` | `skipif` | the user's home cannot be faked by environment |
| `tests/test_release11_socket_routes.py:43` | `skipif` | Unix sockets |
| `tests/test_release11_artifacts_nice.py:75` | `skipif` | engine niceness |

Both `xfail` markers are the ones to watch: when a fix lands, a strict marker turns XPASS
into a failure and forces itself to be removed.

A third marker used to sit here, on `test_clock_armed_rule_fires_only_when_window_is_empty`,
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
