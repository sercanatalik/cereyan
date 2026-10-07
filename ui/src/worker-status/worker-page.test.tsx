// The worker's own status page: its order, health headline, states and banners,
// stale counts when the server is unreachable, the tiles, the flows list with
// its never-run disclosure, older code, event dots, and relative links.
import { render, screen, within } from "@testing-library/react";
import { afterAll, beforeAll, expect, test, vi } from "vitest";
import { WorkerPage } from "./page";
import type { FlowStats, WorkerStatusPayload } from "./types";

const NOW = Date.now() * 1000;

// Relative times ("2 s ago") are measured from NOW; hold the clock there so a slow
// runner does not read "3 s ago". Only Date is faked, so timers still run.
beforeAll(() => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(NOW / 1000);
});
afterAll(() => {
  vi.useRealTimers();
});
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
  expect(screen.getByTestId("tile-failed")).toHaveAttribute("data-muted", "true");
  expect(screen.getByText(/Run #4812 · daily_load/)).toBeInTheDocument();
  expect(screen.getByText("127.0.0.1:51377")).toBeInTheDocument();
  expect(screen.getAllByTestId("flow-bar")).toHaveLength(1);
});

test("a failure colours the Failed tile's dot and number, not its background, and lists its flow first", () => {
  const s = payload({
    flows: [
      { project: "ml", flow: "daily_load", module: "pipelines.etl" },
      { project: "ml", flow: "ml.train", module: "ml" },
    ],
  });
  s.stats?.by_flow.push({
    flow: "ml.train",
    completed: 2,
    failed: 1,
    crashed: 0,
    cancelled: 0,
    running: 0,
    last_completed_at: NOW - 60 * MIN,
  });
  render(<WorkerPage s={s} />);
  const tile = screen.getByTestId("tile-failed");
  expect(tile).not.toHaveAttribute("data-muted");
  expect(tile.className).not.toMatch(/bg-red/);
  expect(within(tile).getByTestId("tile-count")).toHaveTextContent("1");
  expect(within(tile).getByTestId("tile-count").className).toMatch(/text-red-600/);
  expect(within(tile).getByTestId("tile-dot").className).toMatch(/bg-red-500/);
  const rows = screen.getAllByTestId("flow-row");
  expect(rows[0]).toHaveTextContent("ml.train");
  expect(rows[0]).toHaveTextContent("1 ✗");
});

test("a tile at zero is muted; one above zero is lit", () => {
  render(<WorkerPage s={payload()} />);
  const failed = screen.getByTestId("tile-failed");
  expect(failed).toHaveAttribute("data-muted", "true");
  expect(within(failed).getByTestId("tile-count").className).toMatch(/text-muted-foreground/);
  expect(within(failed).getByTestId("tile-dot").className).toMatch(/bg-border/);
  const completed = screen.getByTestId("tile-completed");
  expect(completed).not.toHaveAttribute("data-muted");
  expect(within(completed).getByTestId("tile-dot").className).toMatch(/bg-emerald-500/);
});

test("flows order failing first, then by latest run", () => {
  const flow = (name: string, over: Partial<FlowStats>): FlowStats => ({
    flow: name,
    completed: 1,
    failed: 0,
    crashed: 0,
    cancelled: 0,
    running: 0,
    last_completed_at: null,
    ...over,
  });
  const s = payload({
    flows: ["old", "recent", "broken", "busy"].map((f) => ({ project: "p", flow: f, module: "p" })),
  });
  if (s.stats)
    s.stats.by_flow = [
      flow("old", { last_completed_at: NOW - 90 * MIN }),
      flow("recent", { last_completed_at: NOW - 2 * MIN }),
      flow("broken", { crashed: 1, last_completed_at: NOW - 120 * MIN }),
      flow("busy", { running: 1, last_completed_at: NOW - 150 * MIN }),
    ];
  render(<WorkerPage s={s} />);
  const names = screen.getAllByTestId("flow-row").map((r) => r.querySelector(".font-mono")?.textContent);
  expect(names).toEqual(["broken", "busy", "recent", "old"]);
});

test("flows not yet run fold into one closed disclosure", () => {
  const names = Array.from({ length: 19 }, (_, i) => `flow_${String(i).padStart(2, "0")}`);
  const s = payload({ flows: names.map((f) => ({ project: "p", flow: f, module: "p" })) });
  if (s.stats)
    s.stats.by_flow = names.slice(0, 3).map((f) => ({
      flow: f,
      completed: 1,
      failed: 0,
      crashed: 0,
      cancelled: 0,
      running: 0,
      last_completed_at: NOW - MIN,
    }));
  render(<WorkerPage s={s} />);
  expect(screen.getAllByTestId("flow-row")).toHaveLength(3);
  const more = screen.getByTestId("flows-not-run") as HTMLDetailsElement;
  expect(more.tagName).toBe("DETAILS");
  expect(more.open).toBe(false);
  expect(within(more).getByText("16 more flows can run here but have not yet")).toBeInTheDocument();
  expect(within(more).getByText("flow_18")).toBeInTheDocument();
  // The disclosure follows the rows.
  const rows = screen.getAllByTestId("flow-row");
  expect(rows[2].compareDocumentPosition(more) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

test("the sections come in order", () => {
  render(<WorkerPage s={payload()} />);
  const order = [
    screen.getByRole("heading", { name: "gpu-box-1" }),
    screen.getByTestId("health-headline"),
    screen.getByTestId("fact-server"),
    screen.getByRole("region", { name: "Processors" }),
    screen.getByRole("region", { name: "Runs since start" }),
    screen.getByRole("region", { name: "Flows" }),
    screen.getByRole("region", { name: "Recent activity" }),
    screen.getByRole("region", { name: "Host" }),
  ];
  for (let i = 1; i < order.length; i++)
    expect(order[i - 1].compareDocumentPosition(order[i]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

test("the facts grid names server, id, version, code, heartbeat and uptime with pid", () => {
  render(<WorkerPage s={payload()} />);
  expect(screen.getByTestId("fact-server")).toHaveTextContent("cereyan.internal");
  expect(screen.getByTestId("fact-worker-id")).toHaveTextContent("7");
  expect(screen.getByTestId("fact-version")).toHaveTextContent("3.0.1 · same major version as the server");
  expect(screen.getByTestId("fact-code")).toHaveTextContent("main · a1b2c3d · clean");
  expect(screen.getByTestId("fact-last-heartbeat")).toHaveTextContent("2 s ago");
  expect(screen.getByTestId("fact-up")).toHaveTextContent("3 h 12 m · pid 48213");
});

test("an online worker with no busy processor reads Healthy and idle", () => {
  const s = payload({ engines: [] });
  if (s.stats) s.stats.engines = [];
  render(<WorkerPage s={s} />);
  expect(screen.getByTestId("health-headline").textContent).toMatch(/^Healthy and idle\./);
  expect(screen.getByTestId("health-headline")).toHaveTextContent("any of the 2 flows whose code it has");
});

test("a busy worker says so; an idle slot names the flow it last ran", () => {
  const s = payload();
  const { rerender } = render(<WorkerPage s={s} />);
  expect(screen.getByTestId("health-headline").textContent).toMatch(/^Healthy and busy\. 1 of 2 processors/);
  const next = payload();
  if (next.stats)
    next.stats.engines = [{ ...next.stats.engines[0], status: "idle", run_id: null, flow: null }];
  rerender(<WorkerPage s={next} />);
  expect(screen.getAllByTestId("processor-slot")[0]).toHaveTextContent(/Idle · last ran daily_load/);
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
  expect(screen.getByTestId("health-headline").textContent).toMatch(/^Draining\./);
});

test("a worker not yet registered, or refused, does not read as Online", () => {
  const { unmount } = render(<WorkerPage s={payload({ state: "registering" })} />);
  expect(screen.getByTestId("worker-status")).toHaveTextContent("Registering");
  unmount();
  render(<WorkerPage s={payload({ state: "refused" })} />);
  expect(screen.getByTestId("worker-status")).toHaveTextContent("Refused");
});

test("older code names the module and marks its flow", () => {
  render(<WorkerPage s={payload({ drift: ["pipelines.reports"] })} />);
  expect(screen.getByTestId("worker-status")).toHaveTextContent("Older code");
  expect(screen.getByTestId("worker-banner")).toHaveTextContent("pipelines.reports");
  const weekly = screen.getAllByTestId("flow-row").find((r) => r.textContent?.includes("weekly"));
  expect(weekly).toHaveTextContent("older code · takes no run");
  expect(screen.getByTestId("health-headline")).toHaveTextContent("Running older code in pipelines.reports");
  // It stays in the list, not in the never-run disclosure.
  expect(screen.queryByTestId("flows-not-run")).toBeNull();
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
  expect(within(screen.getByTestId("tile-completed")).getByTestId("tile-count").className).toMatch(
    /opacity-45/,
  );
  expect(screen.getByTestId("fact-up")).not.toHaveTextContent(/as of/);
  expect(screen.getByTestId("health-headline").textContent).toMatch(/^Can't reach the server\./);
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
  expect(within(events[0]).getByTestId("event-dot").className).toMatch(/bg-amber-500/);
  expect(within(events[1]).getByTestId("event-dot").className).toMatch(/bg-sky-500/);
});

test("recent activity shows at most 20 messages, newest first", () => {
  const events = Array.from({ length: 30 }, (_, i) => ({
    at: NOW - (30 - i) * MIN,
    level: "error" as const,
    message: `message ${i}`,
    count: 1,
  }));
  render(<WorkerPage s={payload({ events })} />);
  const shown = screen.getAllByTestId("worker-event");
  expect(shown).toHaveLength(20);
  expect(shown[0]).toHaveTextContent("message 29");
  expect(within(shown[0]).getByTestId("event-dot").className).toMatch(/bg-red-500/);
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
