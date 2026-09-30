// The worker's own status page: its states and banners, stale counts when the
// server is unreachable, the failure tile, older code, and relative links.
import { render, screen, within } from "@testing-library/react";
import { expect, test } from "vitest";
import { WorkerPage } from "./page";
import type { WorkerStatusPayload } from "./types";

const NOW = Date.now() * 1000;
const MIN = 60_000_000;

function payload(over: Partial<WorkerStatusPayload> = {}): WorkerStatusPayload {
  return {
    name: "gpu-box-1",
    server: "https://cereyan.internal",
    server_ui: "https://cereyan.internal/queue?tab=workers",
    worker_id: 7,
    state: "online",
    server_reachable: true,
    heartbeat_secs: 5,
    last_ok_heartbeat_at: NOW - 2_000_000,
    failing_since: null,
    started_at: NOW - 192 * MIN,
    now: NOW,
    processors: 2,
    cpus: 16,
    version: "3.0.1",
    drift: [],
    refused: [],
    flows: [
      { project: "pipelines", flow: "daily_load", module: "pipelines.etl" },
      { project: "pipelines", flow: "weekly", module: "pipelines.reports" },
    ],
    engines: ["e-1"],
    stats: {
      since: NOW - 192 * MIN,
      by_flow: [
        {
          flow: "daily_load",
          completed: 96,
          failed: 0,
          crashed: 0,
          cancelled: 0,
          running: 1,
          last_completed_at: NOW - 4 * MIN,
        },
      ],
      engines: [
        {
          engine_id: "e-1",
          slot: 1,
          module: "pipelines.etl",
          status: "running",
          run_id: 4812,
          flow: "daily_load",
          since_secs: 130,
        },
      ],
    },
    stats_as_of: NOW - 2_000_000,
    events: [
      { at: NOW - 10 * MIN, level: "info", message: "registered as gpu-box-1 (id 7, online)", count: 1 },
      { at: NOW - 5 * MIN, level: "warning", message: "heartbeat failed: server unavailable", count: 3 },
    ],
    host: {
      meta: {
        hostname: "gpu-box-1.lan",
        git: "main · a1b2c3d · clean",
        pid: 48213,
        status_url: "http://127.0.0.1:51377",
      },
      cpus: 16,
      version: "3.0.1",
      labels: { gpu: "true" },
      shared_paths: [],
    },
    ...over,
  };
}

test("an online worker shows its pill, counts, engines and host", () => {
  render(<WorkerPage s={payload()} />);
  expect(screen.getByTestId("worker-status")).toHaveTextContent("Online");
  expect(screen.getByTestId("connection")).toHaveTextContent("connected");
  expect(screen.queryByTestId("worker-banner")).toBeNull();
  expect(within(screen.getByTestId("tile-completed")).getByText("96")).toBeInTheDocument();
  expect(screen.getByTestId("tile-failed")).not.toHaveAttribute("data-alert");
  expect(screen.getByText(/Run #4812 · daily_load/)).toBeInTheDocument();
  expect(screen.getByText("127.0.0.1:51377")).toBeInTheDocument();
  expect(screen.getAllByTestId("flow-bar")).toHaveLength(1);
});

test("failures turn the Failed tile red and count on the flow", () => {
  const s = payload();
  s.stats?.by_flow.splice(0, 1, { ...s.stats.by_flow[0], failed: 1, crashed: 1 });
  render(<WorkerPage s={s} />);
  const tile = screen.getByTestId("tile-failed");
  expect(tile).toHaveAttribute("data-alert", "true");
  expect(within(tile).getByText("2")).toBeInTheDocument();
  expect(screen.getAllByTestId("flow-row")[0]).toHaveTextContent("2 ✗");
});

test("a flow pinned to the server is not counted as runnable here", () => {
  const s = payload();
  s.flows.push({ project: "pipelines", flow: "hold", module: "pipelines.etl", runs_on: "server" });
  render(<WorkerPage s={s} />);
  const hold = screen.getAllByTestId("flow-row").find((r) => r.textContent?.includes("hold"));
  expect(hold).toHaveTextContent("runs on the server only");
  expect(screen.getByText(/2 of 3 flows can run here/)).toBeInTheDocument();
});

test("draining shows its pill and banner", () => {
  render(<WorkerPage s={payload({ state: "draining" })} />);
  expect(screen.getByTestId("worker-status")).toHaveTextContent("Draining");
  expect(screen.getByTestId("worker-banner")).toHaveTextContent(/takes no new run/);
});

test("older code names the module and marks its flow", () => {
  render(<WorkerPage s={payload({ drift: ["pipelines.reports"] })} />);
  expect(screen.getByTestId("worker-status")).toHaveTextContent("Older code");
  expect(screen.getByTestId("worker-banner")).toHaveTextContent("pipelines.reports");
  const weekly = screen.getAllByTestId("flow-row").find((r) => r.textContent?.includes("weekly"));
  expect(weekly).toHaveTextContent("older code · takes no run");
});

test("unreachable keeps the counts, marked with when they are from", () => {
  const s = payload({
    server_reachable: false,
    failing_since: NOW - 3 * MIN,
    stats_as_of: NOW - 3 * MIN,
  });
  render(<WorkerPage s={s} />);
  expect(screen.getByTestId("worker-status")).toHaveTextContent("Server unreachable");
  expect(screen.getByTestId("connection")).toHaveTextContent("reconnecting");
  expect(screen.getByTestId("worker-banner")).toHaveTextContent(
    /Can't reach https:\/\/cereyan.internal since/,
  );
  expect(screen.getByTestId("tile-completed")).toHaveTextContent(/96.*as of/);
  expect(screen.getByTestId("tile-up")).not.toHaveTextContent(/as of/);
  // What engines hold is the server's to say: the last answer is not shown as current.
  expect(screen.queryByText(/Run #4812/)).toBeNull();
  expect(screen.getByText("engine running · run unknown")).toBeInTheDocument();
  expect(screen.getByTestId("tile-running")).toHaveTextContent("1");
});

test("repeated events show their count and level", () => {
  render(<WorkerPage s={payload()} />);
  const events = screen.getAllByTestId("worker-event");
  expect(events[0]).toHaveAttribute("data-level", "warning");
  expect(events[0]).toHaveTextContent("×3");
});

test("links to the page's own data are relative", () => {
  render(<WorkerPage s={payload()} />);
  expect(screen.getByText("status.json").closest("a")).toHaveAttribute("href", "status.json");
  expect(screen.getByText("healthz").closest("a")).toHaveAttribute("href", "healthz");
  expect(screen.getByText(/Open in server/).closest("a")).toHaveAttribute(
    "href",
    "https://cereyan.internal/queue?tab=workers",
  );
});
