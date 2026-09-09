# Install

```bash
pip install cereyan
```

Cereyan needs Python 3.11 or newer. The wheel is self-contained: it has no runtime dependencies and includes the Rust core and the web UI. Wheels are published for macOS (Apple silicon and Intel), Linux (x86_64 and aarch64, manylinux 2.28), and Windows (x86_64).

=== "pip"

    ```bash
    pip install cereyan
    ```

=== "uv"

    ```bash
    uv add cereyan
    ```

Check the install:

```bash
cereyan --help
python -c "import cereyan; print(cereyan.__version__)"
```

## What gets created

The first run creates the runtime home, `~/.cereyan` by default, holding the SQLite database and, later, `server.json` while a server runs, `secret.key` once a secret variable exists, and `storage/` for persisted results. Set `CEREYAN_HOME` or pass `--home` to put it elsewhere; see [Engines and the home directory](../concepts/engines-and-home.md).

## Windows

The Unix socket listener and engine niceness are Unix-only options, rejected or ignored with a message. Two behaviours are weaker on Windows, both of them signal-based:

| Behaviour | On Windows |
|---|---|
| A flow's `timeout_seconds` offline (`python pipeline.py`, `cereyan run`) | Accepted and ignored; the flow runs unbounded. A served run is timed out by the server. |
| Cancelling a flow blocked in a sleep or an I/O call | The engine is ended rather than interrupted inside the flow, so `on_cancellation` hooks do not run. |

Everything else — the server, the UI, schedules, engines, rules, artifacts, variables, and the MCP server — works the same.

Next: the [Quickstart](quickstart.md).
