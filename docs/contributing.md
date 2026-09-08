# Contributing

Cereyan is a Cargo workspace (`crates/`) with a Python package (`python/cereyan`), a React UI (`ui/`), and this documentation (`docs/`), built as one wheel by maturin. Design decisions and the phased history live in `roadmap.md`.

## Set up

You need Rust (stable), Python 3.11 or newer with [uv](https://docs.astral.sh/uv/), Node 22, and [just](https://github.com/casey/just).

```bash
git clone https://github.com/sercanatalik/cereyan
cd cereyan
just ui      # install UI dependencies and build ui/dist, which the server crate embeds
just dev     # uv sync and maturin develop: builds the extension in place
```

## Everyday commands

| Command | Does |
|---|---|
| `just test` | Rust tests, Python tests, documentation tests, UI tests. Correctness only: tests that assert a wall-clock ceiling are marked `#[ignore]` or `@pytest.mark.performance` and run in `just bench` instead, because a ceiling calibrated on a developer machine fails on a shared CI runner and says nothing about correctness |
| `just lint` | rustfmt and clippy, Python compile check, docstring check, generated-page checks, UI lint and client drift check |
| `just docs` | Regenerate the reference pages and example pages, check docstrings, build the site into `site/` in strict mode, print the tested-block summary |
| `just docs-serve` | Serve the docs locally with live reload |
| `just docs-test` | Execute every Python block under `docs/` and every file under `examples/` |
| `just demo` | Serve `examples/` on a temporary home at http://127.0.0.1:4200 |
| `just service-sync` | Regenerate the UI's typed client from `ui/openapi.snapshot.json` |
| `just bench` | Criterion benchmarks, the wall-clock tests `just test` skips, and the end-to-end benchmark against the checked-in baseline |
| `just soak` | The one-hour overlap soak: fifteen scheduled flows whose runs outlast their interval, checked against the overlap invariants at the end. `just soak --quick` takes twelve minutes; `--keep` leaves the UI up. Not a regression gate and not on the default CI path |
| `just build` | Build a release wheel into `dist/` |

## Where things are

| Path | Contents |
|---|---|
| `crates/core` | The model and the state rules: no I/O, no async |
| `crates/store` | SQLite: writer thread, read pool, migrations, quarantine |
| `crates/rules` | Rule matching and templating |
| `crates/server` | axum: API, UI serving, scheduler, supervisor, rules engine, MCP, retention |
| `crates/py` | The pyo3 module `cereyan._core` |
| `python/cereyan` | Decorators, parameters, targets, results, client, CLI, engine child, MCP stdio proxy |
| `ui/` | React, Tanstack, shadcn; embedded in the wheel at build time |
| `docs/`, `examples/`, `scripts/` | This site, the literate examples, the generators; `docs/AGENTS.md` is the authoring guide |
| `tests/` | Python tests; `tests/docs/` holds the documentation fixtures, `tests/mcp_snapshot.json` the MCP surface |
| `benches/` | Benchmarks, per-platform baselines, and the overlap soak (`soak_overlap.py`) |

## The snapshots

`ui/openapi.snapshot.json` is the contract between the server, the UI client, and the HTTP API reference. The Python suite asserts the live server's document matches it, the UI build fails when the generated client drifts, and `just lint` fails when the reference page is stale. After changing a route:

```bash
CEREYAN_UPDATE_SNAPSHOTS=1 uv run pytest tests/test_server_api.py -k openapi
just service-sync
just docs
```

`tests/mcp_snapshot.json` does the same for the MCP surface: the `initialize` handshake, every tool with its schema, the resource templates, the prompts, and the keys each tool returns. `scripts/gen_mcp_reference.py` renders `docs/reference/mcp.md` from it and `scripts/mcp_reference_template.md`. After changing `crates/server/src/mcp.rs`:

```bash
CEREYAN_UPDATE_SNAPSHOTS=1 uv run pytest tests/test_agent_mcp.py -k snapshot
just docs
```

## Writing documentation

`docs/AGENTS.md` is the authoring guide: page kinds, the section budgets, the vocabulary, the style, how code blocks are tested and marked, which pages are generated, and how to redirect a moved page. In short: every Python block runs in tests, every feature is documented once in the section its kind of content belongs to, and every change's tasks include a documentation task.

## Release checklist

1. `just lint` and `just test` are green on the release commit.
2. `just bench` shows no target regressing more than 20 percent against `benches/baseline.<platform>.json` (`just bench-baseline` writes it for the current platform); update the baseline in the same change when a regression is intentional. Run this on calibrated hardware: CI's benchmark job passes `--no-ceilings`, because a shared runner has no baseline and cannot meet absolute targets, so it reports rather than gates.
3. Bump the version in `pyproject.toml`, `Cargo.toml` (workspace), `python/cereyan/__init__.py`, and `ui/package.json`, and regenerate `Cargo.lock`. One version, four files: `test_the_four_version_strings_agree` fails when one of them is missed. The version is also in the OpenAPI document's `info` block, so refresh the snapshot and the reference page it feeds:

    ```bash
    CEREYAN_UPDATE_SNAPSHOTS=1 uv run pytest tests/test_server_api.py -k openapi
    just service-sync
    just docs
    ```
4. Move the `CHANGELOG.md` entries under a heading for the new version. Every change to the Python API, the HTTP API, the CLI, the MCP surface, the UI, or the behaviour of a running server needs an entry, and an entry that removes or reverses documented behaviour says what a reader relying on it must do.
5. CI builds the UI, then wheels for macOS arm64 and x86_64, Linux x86_64 and aarch64 (manylinux 2.28), and Windows x86_64, plus the sdist.
6. The smoke stage installs each wheel into a fresh virtual environment on its platform and runs `scripts/smoke.sh`: import, offline run, `cereyan runs ls`.
7. Tag the release as `v<version>`. The `docs` job uploads the built site as a Pages artifact, `deploy-docs` publishes it, and only then does `publish` upload the wheels and the sdist to PyPI — so the package page never goes live linking to a site that does not yet exist.

### One-time setup

Two things live in GitHub rather than in the repository, and a fork or a restored repository needs both before its first tagged release.

**GitHub Pages.** The site is deployed from a build artifact, not from a `gh-pages` branch, so Pages must be set to the workflow build type. This works on a repository that has never deployed anything:

```bash
gh api -X POST repos/<owner>/<repo>/pages -f build_type=workflow
```

The `github-pages` environment GitHub creates alongside it allows deployments only from the default branch, which would reject every tag-triggered deploy. Add a tag policy:

```bash
gh api -X POST repos/<owner>/<repo>/environments/github-pages/deployment-branch-policies \
  -f name='v*' -f type=tag
```

**PyPI.** Publishing uses [trusted publishing](https://docs.pypi.org/trusted-publishers/): PyPI holds a publisher for this repository, workflow `ci.yml`, environment `pypi`, and CI mints a short-lived token for it, so no API token is stored in the repository. Set the publisher up on PyPI and create the `pypi` environment in the repository settings. Nothing else in CI needs credentials.

## License

MIT. UI components adapted from Prefect are listed in `NOTICE` and remain under the Apache License 2.0 of their origin.
