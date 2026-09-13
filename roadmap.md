# Cereyan roadmap

What is left to do. Everything shipped is in `CHANGELOG.md`, and the phases it was built in are in this file's git history. What cereyan will not do, and why, is on [Limitations](docs/design/limitations.md); the performance targets are on [Performance](docs/design/performance.md).

## Interrupt a running flow on Windows

Two ways cereyan stops a flow from inside its own process do not work on Windows. Served timeouts are not one of them: the server records `TimedOut` and ends the engine on every platform.

- An offline flow's `timeout_seconds` is accepted and ignored. `_FlowTimeout` arms only when `signal.setitimer` exists, so a flow run by `python pipeline.py` or `cereyan run` runs unbounded. Task timeouts are unaffected.
- A cancel cannot interrupt a sleeping flow. The fallback, `_thread.interrupt_main()`, only sets a flag CPython checks between bytecodes, so the run ends when the supervisor terminates the engine after the 10 second grace period.

The fix is `signal.raise_signal(SIGINT)` from a watcher thread: it goes through CPython's C-level handler, which on Windows also sets the event `time.sleep` waits on, so it wakes a sleeping main thread with no console involved. Windows still cannot interrupt most blocking I/O, and the specs will say so rather than imply parity.

State: proposal, design, specs and tasks written (OpenSpec change `windows-interrupt-running-flow`). The first task is the cancel change plus a Windows CI run, which the strict `xfail` markers turn into the experiment. Details are in `WINDOWS.md`.
