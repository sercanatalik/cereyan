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

Everything works on Windows except the Unix socket listener and engine niceness, which are Unix-only options and are rejected or ignored with a message.

Next: the [Quickstart](quickstart.md).
