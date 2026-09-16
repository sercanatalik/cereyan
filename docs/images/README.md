# Screenshots

Every image here is written by one script, which serves `examples/` on a free port with a temporary home, gives it some history, and captures each page at 1440x900:

```bash
just ui && just dev                       # a built UI and the extension
node scripts/capture_screenshots.mjs      # all images
node scripts/capture_screenshots.mjs flows flow-detail --out /tmp/shots   # some, elsewhere
```

It needs Node 22 and Chrome (set `CHROME` if Chrome is not in a standard place), and it never touches a server already running on 4200. Check each image by eye before committing, and keep each under 300 KB; the script flags a larger one.

| Image | Shows |
|---|---|
| `dashboard.png`, `dashboard-dark.png` | The Dashboard in each theme, with a paused and a failed run under Needs attention |
| `runs.png` | Runs with the `examples` project's section open |
| `run-detail.png`, `run-detail-dark.png` | An `etl` run's Logs tab in each theme |
| `run-timeline.png` | The same run's Timeline tab |
| `flows.png` | All flows banded by group, with `daily_etl`'s row menu open |
| `flow-detail.png` | `daily_etl`'s Upcoming tab with a skipped fire and projected fires |
| `skip-dialog.png`, `reschedule-dialog.png` | The Skip upcoming runs dialog set to three runs, and Reschedule with the time moved to 07:30 |
| `events.png` | The Events feed with a `run.failed` event open |
| `artifacts.png`, `rules.png`, `variables.png`, `settings.png` | Those pages with the seeded artifacts, rules, and variables |

Every file here must be referenced from a page with alt text that says what the picture shows. When the script changes what a picture shows, update its alt text on the pages that use it.
