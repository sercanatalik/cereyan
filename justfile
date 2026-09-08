set shell := ["bash", "-cu"]

# Build a release wheel into dist/ (builds the UI first)
build: ui
    uv run maturin build --release --out dist

# Build the extension in place for development
dev:
    uv sync
    uv run maturin develop --uv

# Install UI dependencies and build ui/dist (embedded by the server crate)
ui:
    cd ui && npm ci --no-audit --no-fund && npm run build

# Regenerate the UI's typed client from the checked-in OpenAPI snapshot
service-sync:
    cd ui && npm run service-sync -- --from openapi.snapshot.json

# Run Rust, Python, documentation, and UI tests (wall-clock ceilings live in `just bench`)
test:
    cargo test --workspace
    uv run pytest -q -m "not performance"
    just docs-test
    cd ui && npx vitest run

# Execute every Python block under docs/ and every file under examples/
docs-test:
    uv run --group docs pytest --markdown-docs --markdown-docs-syntax superfences --ignore=docs/examples --ignore-glob='*/conftest.py' docs tests/docs -q

# Lint Rust, Python, docs, and UI
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    uv run python -m compileall -q python tests scripts
    uv run python scripts/check_docstrings.py
    uv run --with pyyaml python scripts/check_workflows.py
    uv run python scripts/gen_cli_reference.py --check
    uv run python scripts/gen_http_reference.py --check
    uv run python scripts/gen_mcp_reference.py --check
    uv run python scripts/gen_examples.py --check
    uv run python scripts/docs_blocks.py
    cd ui && npx biome check src && node scripts/service-sync.mjs --from openapi.snapshot.json --check

# Serve the example pipeline with a temporary home
demo:
    CEREYAN_HOME=$(mktemp -d) uv run cereyan serve examples --port 4200

# Build the docs site into site/: generators, docstring check, strict build, block summary
docs:
    uv run python scripts/check_docstrings.py
    uv run python scripts/gen_cli_reference.py
    uv run python scripts/gen_http_reference.py
    uv run python scripts/gen_mcp_reference.py
    uv run python scripts/gen_examples.py
    uv run --group docs mkdocs build --strict
    uv run python scripts/docs_blocks.py

# Serve the docs locally with live reload
docs-serve:
    uv run --group docs mkdocs serve

# Run the criterion benchmarks and the end-to-end benchmark against the baseline
bench:
    cargo bench --workspace
    cargo test --workspace -- --ignored
    uv run pytest -q -m performance
    uv run python benches/e2e.py

# Refresh benches/baseline.<platform>.json after an intentional performance change
bench-baseline:
    uv run python benches/e2e.py --update-baseline

# Run the one-hour overlap soak (pass --quick for twelve minutes, --keep to leave the UI up)
soak *args:
    uv run python benches/soak_overlap.py {{args}}
