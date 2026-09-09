# Screenshots

Captured by hand from a demo server; there is no automated regeneration.

1. `just demo` starts a server on a temporary home serving `examples/`, at http://127.0.0.1:4200.
2. Run a few example flows (the Run button on the Flows page, or `python examples/daily_etl.py` in another shell) so the pages have data, including one failed run and one paused run.
3. Use a 1440x900 browser window and capture each page as PNG named after the page: `dashboard`, `runs`, `run-detail`, `run-timeline`, `flows`, `flow-detail`, `events`, `artifacts`, `rules`, `variables`, `settings`, plus `dashboard-dark` and `run-detail-dark` in the dark theme.
4. Crop to the browser viewport and keep each file under 300 KB.

Every file here must be referenced from a page (checked in review) with alt text that says what the picture shows.

The current set was captured on the ui-redesign change (2026-09-06) with the top bar shell; `flows.png` and `runs.png` were recaptured on add-flow-groups (2026-09-09) with the collapsible groups. `dashboard-dark.png` and `run-detail-dark.png` show the dark theme; the dark captures come from a browser whose system theme is dark, since the page follows `prefers-color-scheme` until the toggle is used. `artifacts.png` shows the Artifacts page.
