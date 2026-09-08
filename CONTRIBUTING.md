# Contributing

The contributing guide lives with the rest of the documentation:

**<https://sercanatalik.github.io/cereyan/contributing/>**

It covers the toolchain (Rust stable, Python 3.11+ with uv, Node 22, just), the everyday
`just` recipes, the repository layout, the OpenAPI and MCP snapshots, and the release
checklist. The source is [`docs/contributing.md`](docs/contributing.md); before adding or
changing a page under `docs/`, read [`docs/AGENTS.md`](docs/AGENTS.md).

In short:

```bash
git clone https://github.com/sercanatalik/cereyan
cd cereyan
just ui      # install UI dependencies and build ui/dist, which the server crate embeds
just dev     # uv sync and maturin develop: builds the extension in place
just test    # Rust, Python, documentation, and UI tests
just lint    # rustfmt, clippy, docstring and generated-page checks, UI lint
```

Please open an issue before a large change, so we can agree on the shape of it first.
Security problems go to [`SECURITY.md`](SECURITY.md), not to a public issue.
