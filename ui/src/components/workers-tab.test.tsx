// Queue › Workers: the registered workers, their status and host, their schedule,
// drain and forget; and the Line tab's processors grouped by host.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ProjectProvider } from "@/lib/project";
import { routeTree } from "@/routeTree.gen";
import { workerStatus } from "./workers-tab";

const fetchMock = vi.hoisted(() => {
  const fn = vi.fn();
  globalThis.fetch = fn as unknown as typeof fetch;
  const NativeRequest = globalThis.Request;
  globalThis.Request = class extends NativeRequest {
    constructor(input: RequestInfo | URL, init?: RequestInit) {
      super(typeof input === "string" && input.startsWith("/") ? `http://localhost${input}` : input, init);
    }
  } as typeof Request;
  return fn;
});

vi.mock("@/lib/live", () => ({
  useLiveUpdates: () => ({ status: "live" }),
  useLiveEvent: () => {},
  LiveProvider: ({ children }: { children: React.ReactNode }) => children,
}));

const NOW = Date.now() * 1000;
const calls: { method: string; path: string }[] = [];

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

const worker = (id: number, name: string, extra: Record<string, unknown> = {}) => ({
  id,
  name,
  version: "3.0.0",
  cpus: 16,
  processors: 2,
  labels: { gpu: "true" },
  shared_paths: ["/mnt/lake"],
  meta: {
    hostname: `${name}.corp.internal`,
    address: "10.0.4.21",
    platform: "Linux-6.5",
    arch: "x86_64",
    gpus: ["NVIDIA L4"],
    python: "3.12.7",
    checkout: "/opt/pipelines",
    git: "main · def456 · clean",
    connection: "TLS",
    auth: "server token",
  },
  state: "online",
  registered_at: NOW - 3_600_000_000,
  last_seen_at: NOW - 2_000_000,
  running: 1,
  idle: 1,
  flows: 3,
  drift: [],
  ...extra,
});

function queue() {
  return {
    processors: {
      count: 1,
      cap: 8,
      source: "default",
      load: 0.2,
      hosts: [
        { host: "server", count: 1, busy: 1, state: "online" },
        { host: "gpu-1", count: 2, busy: 1, state: "online" },
      ],
      items: [
        { id: "e1", host: "server", slot: 1, status: "running", module: "etl", run_id: 40, since_secs: 60 },
        { id: "w1-1", host: "gpu-1", slot: 1, status: "running", module: "ml", run_id: 41, since_secs: 30 },
      ],
    },
    in_line: [],
    more: 0,
    joining: [],
    paused_loops: [],
  };
}

beforeEach(() => {
  calls.length = 0;
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const method = init?.method ?? request?.method ?? "GET";
    const p = url.pathname;
    calls.push({ method, path: p });
    if (p === "/api/queue") return json(queue());
    if (p === "/api/workers" && method === "GET")
      return json([
        worker(1, "gpu-1"),
        worker(2, "mac-mini", { drift: ["etl.orders", "ml.pipeline"], running: 0 }),
        worker(3, "etl-3", { state: "offline", running: 0, idle: 0 }),
      ]);
    if (/^\/api\/workers\/\d+\/timeline$/.test(p))
      return json({
        host: "gpu-1",
        since: NOW - 6 * 3_600_000_000,
        runs: [
          {
            run_id: 98,
            flow: "train_model",
            processor: 1,
            state: "Completed",
            start: NOW - 3_600_000_000,
            end: NOW - 1_800_000_000,
          },
          {
            run_id: 99,
            flow: "train_model",
            processor: 2,
            state: "Failed",
            start: NOW - 900_000_000,
            end: NOW - 600_000_000,
          },
        ],
        next: [{ run_id: 120, flow: "ingest_orders", processor: null, state: null, start: NOW, end: null }],
      });
    if (/^\/api\/workers\/\d+\/(drain|resume)$/.test(p)) return json([]);
    if (/^\/api\/workers\/\d+$/.test(p) && method === "DELETE") return new Response(null, { status: 204 });
    if (p === "/api/settings")
      return json({ max_engines: 1, cpu_cap: 8, engine_saturation_risk: false, saturation_flows: [] });
    if (p === "/api/server")
      return json({ title: "cereyan", queued: 0, engines: [], paused: null, exposed: false, auth: true });
    return json({ error: `unmocked ${method} ${p}` }, 404);
  });
});

/** The list once the workers have loaded: the server and three workers. */
async function loadedRows() {
  await waitFor(() => expect(screen.getAllByTestId("worker-row")).toHaveLength(4));
  return screen.getAllByTestId("worker-row");
}

function mount(path: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: [path] }),
    context: { queryClient: client },
  });
  render(
    <QueryClientProvider client={client}>
      <ProjectProvider>
        <RouterProvider router={router} />
      </ProjectProvider>
    </QueryClientProvider>,
  );
}

test("a worker's status reads offline, draining, older code, then online", () => {
  expect(workerStatus({ state: "offline", drift: ["a"] })).toBe("offline");
  expect(workerStatus({ state: "draining", drift: [] })).toBe("draining");
  expect(workerStatus({ state: "online", drift: ["a"] })).toBe("drift");
  expect(workerStatus({ state: "online", drift: [] })).toBe("online");
});

test("the Line tab groups processors by host", async () => {
  mount("/queue");
  const group = await screen.findByTestId("host-group");
  expect(group).toHaveTextContent("gpu-1");
  expect(
    within(group)
      .getAllByTestId("processor-tile")
      .map((t) => t.dataset.status),
  ).toEqual(["running", "available"]);
});

test("the Workers tab lists the server first, then each worker with its status", async () => {
  mount("/queue?tab=workers");
  const rows = await loadedRows();
  expect(rows.map((r) => within(r).getByTestId("worker-status").textContent)).toEqual([
    "Server",
    "Online",
    "Older code",
    "Offline",
  ]);
  expect(rows[1]).toHaveTextContent("gpu-1.corp.internal · 10.0.4.21");
});

test("a worker's detail shows its host, schedule, and drain", async () => {
  mount("/queue?tab=workers");
  const rows = await loadedRows();
  fireEvent.click(rows[1]);
  const detail = await screen.findByTestId("worker-detail");
  expect(detail).toHaveTextContent("NVIDIA L4");
  expect(detail).toHaveTextContent("/opt/pipelines");
  expect(detail).toHaveTextContent("main · def456 · clean");
  await waitFor(() => expect(within(detail).getAllByTestId("timeline-run")).toHaveLength(2));
  expect(within(detail).getByTestId("timeline-next")).toHaveTextContent("ingest_orders #120");
  fireEvent.click(within(detail).getByRole("button", { name: "Drain" }));
  await waitFor(() => expect(calls).toContainEqual({ method: "POST", path: "/api/workers/1/drain" }));
});

test("older code names the modules, and an offline worker can be forgotten", async () => {
  mount("/queue?tab=workers");
  const rows = await loadedRows();
  fireEvent.click(rows[2]);
  const drifted = await screen.findByTestId("worker-detail");
  expect(drifted).toHaveTextContent("Code differs from the server in etl.orders, ml.pipeline");
  fireEvent.click(rows[3]);
  await waitFor(() => expect(screen.getByTestId("worker-detail")).toHaveTextContent("Offline since"));
  fireEvent.click(screen.getByRole("button", { name: "Forget worker" }));
  await waitFor(() => expect(calls).toContainEqual({ method: "DELETE", path: "/api/workers/3" }));
});
