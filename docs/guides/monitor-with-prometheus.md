# How to monitor with Prometheus

The server exposes its state in the Prometheus text format at `/api/metrics`, and nothing else is needed on the machine: no exporter, no agent. Scrape it like any other target.

## Scrape it

With a token set, the endpoint needs it like every other `/api/*` route:

```yaml
scrape_configs:
  - job_name: cereyan
    metrics_path: /api/metrics
    authorization:
      credentials_file: /etc/prometheus/cereyan-token
    static_configs:
      - targets: ["127.0.0.1:4200"]
```

To scrape without a credential, make the endpoint public; it then answers like `/api/health`, while everything else stays behind the token:

```toml
[server]
metrics_public = true
```

Or `--metrics-public`, `CEREYAN_METRICS_PUBLIC`, or `app.serve(metrics_public=True)`. A public endpoint reveals flow names, run counts, and resource use to anyone who can reach the port, so keep the server on loopback or behind a proxy that authenticates. The host check still applies: a scraper that reaches the server by a name rather than an IP address needs that name in `allowed_hosts`; see [Secure the server](secure-the-server.md#reach-the-server-under-another-name).

## What it reports

| Series | Kind | Meaning |
|---|---|---|
| `cereyan_runs{state}`, `cereyan_flow_runs{project,flow,state}`, `cereyan_task_runs{state}` | gauge | Counts by state type, overall and per flow. |
| `cereyan_active_runs` | gauge | Non-terminal runs the server tracks. |
| `cereyan_queue_depth` | gauge | Runs waiting for an engine. |
| `cereyan_engines{status}`, `cereyan_engines_max` | gauge | Busy and idle engines, and the pool size. |
| `cereyan_resource_total{resource}`, `cereyan_resource_used{resource}` | gauge | `[resources]` totals and what running runs hold. |
| `cereyan_rule_firings_total{rule}` | counter | How often each rule has fired. |
| `cereyan_schedules` | gauge | Schedules across every flow. |
| `cereyan_schedule_start_delay_seconds` | histogram | From a run's scheduled time to its start: a rising mean means the pool is behind. |
| `cereyan_resource_wait_seconds` | histogram | Time runs spent in AwaitingResource. |
| `cereyan_store_commits_total`, `cereyan_store_commit_seconds`, `cereyan_store_write_queue` | counter, histogram, gauge | The writer's commits, their latency, and the writes queued for it. |
| `cereyan_database_bytes`, `cereyan_wal_bytes` | gauge | File sizes; retention and backups are on the [clean-up page](clean-up-the-store.md). |
| `cereyan_uptime_seconds`, `cereyan_info{version}` | gauge | Since start, and the version. |

Two alerts cover most of what goes wrong: `cereyan_queue_depth` staying above zero while `cereyan_engines{status="idle"}` is zero means the pool is saturated, and `increase(cereyan_runs{state="Failed"}[1h])` catches a flow that started failing. The dashboard shows the last hour of queue depth as a sparkline from the same data, served as JSON at `/api/metrics/history`.

Related: [Run the server as a service](run-as-a-service.md) · [Configuration](../reference/configuration.md)
