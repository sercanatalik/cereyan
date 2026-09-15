# Screenshots

Captured by hand from a demo server; there is no automated regeneration.

1. `just demo` starts a server on a temporary home serving `examples/`, at http://127.0.0.1:4200.
2. Run a few example flows (the Run button on the Flows page, or `python examples/daily_etl.py` in another shell) so the pages have data, including one failed run and one paused run.
3. Use a 1440x900 browser window and capture each page as PNG named after the page: `dashboard`, `runs`, `run-detail`, `run-timeline`, `flows`, `flow-detail`, `skip-dialog`, `reschedule-dialog`, `events`, `artifacts`, `rules`, `variables`, `settings`, plus `dashboard-dark` and `run-detail-dark` in the dark theme.
4. Crop to the browser viewport and keep each file under 300 KB.

Every file here must be referenced from a page (checked in review) with alt text that says what the picture shows.

The current set was captured on the ui-redesign change (2026-09-06) with the top bar shell; `flows.png` and `runs.png` were recaptured on add-flow-groups (2026-09-09) with the collapsible groups, and `flows.png` and `flow-detail.png` again on skip-and-reschedule (2026-09-13) beside the new `skip-dialog.png` and `reschedule-dialog.png`: a scheduled flow's row menu open, the Upcoming tab with a skipped fire of `daily_etl` and the projected fires below it, the Skip dialog set to the next three runs, and the Reschedule dialog with the time moved to 07:30. `settings.png` was recaptured on configurable-ui-title (2026-09-14) with the Interface card and its Title field above the server details. `dashboard.png`, `dashboard-dark.png`, `events.png`, and `settings.png` were recaptured on wide-screen-layout (2026-09-15) with the shared frame: the Dashboard's Recently completed table, the full-height Events feed with its day row and the detail panel open on a selected event, and Settings on its General tab in the 1080 px column. `dashboard-dark.png` and `run-detail-dark.png` show the dark theme; the dark captures come from a browser whose system theme is dark, since the page follows `prefers-color-scheme` until the toggle is used. `artifacts.png` shows the Artifacts page.
