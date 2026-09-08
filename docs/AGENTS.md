# Documentation authoring guide

For anyone — person or agent — adding or changing a page under `docs/`. Read it before you write.

This page is the authoring contract for `docs/`: the rules below are the ones `just lint` and the documentation tests enforce. Reader-facing contributor instructions live in [Contributing](contributing.md); this page is not part of the built site.

## Layout

| Path | Holds |
|---|---|
| `docs/get-started/` | Install, quickstart, tour of the UI |
| `docs/concepts/` | One page per idea in the vocabulary |
| `docs/guides/` | One page per goal, grouped in `mkdocs.yml` under Reliability, Running work, Reacting, Extending, Operating |
| `docs/reference/` | Complete catalogues; four generated, four hand-authored |
| `docs/examples/` | Generated from `examples/*.py`; edit the Python file, never the page |
| `docs/design/` | Architecture, limitations, performance targets |
| `docs/images/` | Screenshots; see `docs/images/README.md` for how they are captured |
| `tests/docs/` | The fixtures every code block runs against |
| `scripts/` | The generators and the documentation checks |

## Generated pages — never edit

Edit the source of truth and run `just docs`. `just lint` fails when a committed page is stale.

| Page | Generator | Source of truth |
|---|---|---|
| `docs/reference/cli.md` | `scripts/gen_cli_reference.py` | The argparse parser in `python/cereyan/cli.py` |
| `docs/reference/http-api.md` | `scripts/gen_http_reference.py` | `ui/openapi.snapshot.json` |
| `docs/reference/mcp.md` | `scripts/gen_mcp_reference.py` | `tests/mcp_snapshot.json` and `scripts/mcp_reference_template.md` |
| `docs/examples/*.md` | `scripts/gen_examples.py` | `examples/*.py` |

`docs/reference/python-api.md` is hand-authored but its content is rendered by mkdocstrings from the docstrings in `python/cereyan/`: edit the docstring, not the page. `scripts/check_docstrings.py` fails when a public name has none.

Two snapshots feed generators and are refreshed deliberately, never by hand:

```bash
CEREYAN_UPDATE_SNAPSHOTS=1 uv run pytest tests/test_server_api.py -k openapi   # ui/openapi.snapshot.json
CEREYAN_UPDATE_SNAPSHOTS=1 uv run pytest tests/test_agent_mcp.py -k snapshot   # tests/mcp_snapshot.json
just docs
```

## Page kinds

One kind of content per page. A feature is explained once, in the section its kind belongs to, and linked from the others.

| Kind | Title | Opens with | Budget | Never |
|---|---|---|---|---|
| Concept | The noun (`Schedules`) | A runnable Python block | 3 to 6 `##` sections, under 70 lines | Procedures — link to the guide |
| Guide | `How to <goal>` | The smallest runnable snippet | 2 to 7 `##` sections, under 105 lines | Explaining what a flow, task, run, or state is — link to the concept |
| Reference | The subject (`Events`) | The catalogue | Complete for its subject; generated pages are exempt | Narrative |
| Example | The scenario | The source link | Generated | Any hand edit |
| Design | The subject | The claim | 2 to 5 `##` sections | Roadmap promises |

Every page ends with a `Related:` line linking two or three neighbours. Every hand-authored Reference page names the file in the tree it mirrors.

## Vocabulary

App, flow, task, run, task run, state, schedule, parameter, target, resource, backfill, artifact, variable, event, rule. State types: Scheduled, Pending, Running, Completed, Failed, Cancelled, Crashed, Paused, Cancelling. Named sub-states: Late, AwaitingRetry, Retrying, TimedOut, Cached, Skipped.

Never write "deployment", "job", "DAG", or "workflow" for a flow. "Deployment" appears only in `docs/guides/migrate.md`, explaining the Prefect term.

## Style

- Second person, present tense. Lead with what the reader can do.
- No marketing adjectives, no "simply", no "just", no future promises.
- British spelling, sentence case in headings, no full stop in a heading.
- Prefer a table to a list and a list to a paragraph when the content is enumerable.
- Name a limitation where a reader would hit it rather than hiding it in `docs/design/limitations.md`.

## Code blocks

Every Python block under `docs/` is executed by `just docs-test`. Blocks under `docs/examples/` are not: their source files are tested directly.

| Marker | Means |
|---|---|
| ```` ```python ```` | Runs offline against the `docs_block` fixture: a temporary `CEREYAN_HOME`, a temporary working directory, and a fresh default App per block |
| ```` ```{.python fixture:served} ```` | Runs against `served`, one `cereyan serve examples/` process for the session; use `served.url` |
| ```` ```{.python continuation} ```` | Continues the previous block in the same namespace |
| ```` ```{.python notest} ```` | Not executed; the line immediately above must be `<!-- notest: reason -->` |

`scripts/docs_blocks.py` counts the blocks, fails on a `notest` block without a reason, and fails when this page is missing or out of step with the generated pages above.

## Moved pages

A page that moves keeps its old URL: add `old/path.md: new/path.md` to `redirect_maps` in `mkdocs.yml` and leave it there. Every entry there is a URL someone may have bookmarked.

## Commands

| Command | Use |
|---|---|
| `just docs` | Run the generators and the docstring check, build the site strictly, print the block summary |
| `just docs-test` | Execute every Python block under `docs/` and every file under `examples/` |
| `just docs-serve` | Live reload while writing |
| `just lint` | Includes every generator with `--check` and the block and guide checks |

## Before you finish

1. Your change names the pages you added or changed, or says no documentation change is needed and why.
2. Every new option, route, event, state, key, or MCP tool appears on a Reference page, and on a Concept or Guide page where a reader would use it.
3. `CHANGELOG.md` has an entry under Unreleased for anything a reader would notice.
4. `just docs-test` and `just lint` pass.
