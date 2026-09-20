// The compare page: parameter diff, first divergence, new errors, from /api/runs/compare.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { render, screen } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ProjectProvider } from "@/lib/project";
import { routeTree } from "@/routeTree.gen";

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

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function run(id: number, state: string, message: string | null = null) {
  return {
    id,
    external_id: `r${id}`,
    flow_id: 1,
    flow_name: "etl",
    project: "proj",
    group: "proj",
    name: `run-${id}`,
    parameters: {},
    tags: [],
    attributes: {},
    state: { type: state, name: state, message, details: {}, timestamp: 1 },
    failure_count: 0,
    crash_count: 0,
    created_at: 1_000_000,
    start_time: 1_000_000,
    end_time: 4_000_000,
    total_run_time: 3_000_000,
    engine_pid: null,
    engine_id: null,
    created_by: "client",
    report_seq: 0,
  };
}

const comparison = {
  left: run(1, "Completed"),
  right: run(2, "Failed", "bad day"),
  same_flow: true,
  parameters: [
    { key: "day", left: "2026-09-01", right: "2026-09-02", changed: true },
    { key: "n", left: 1, right: 1, changed: false },
  ],
  attributes: [],
  duration: { left: 3_000_000, right: 3_000_000, delta: 0 },
  tasks: [
    {
      key: "load-0",
      name: "load",
      left: {
        id: 1,
        state: { type: "Completed", name: "Completed", message: null, details: {}, timestamp: 1 },
        duration: 1_000_000,
      },
      right: {
        id: 3,
        state: { type: "Completed", name: "Completed", message: null, details: {}, timestamp: 1 },
        duration: 3_500_000,
      },
      state_changed: false,
      delta: 2_500_000,
    },
    {
      key: "check-0",
      name: "check",
      left: {
        id: 2,
        state: { type: "Completed", name: "Completed", message: null, details: {}, timestamp: 1 },
        duration: 1_000_000,
      },
      right: {
        id: 4,
        state: { type: "Failed", name: "Failed", message: "bad day", details: {}, timestamp: 1 },
        duration: 100_000,
      },
      state_changed: true,
      delta: -900_000,
    },
  ],
  first_divergence: "check-0",
  new_errors: ["bad day"],
  artifacts: [],
  summary: {
    parameters_changed: 1,
    attributes_changed: 0,
    tasks_state_changed: 1,
    tasks_duration_changed: 1,
    new_errors: 1,
    artifacts_changed: 0,
  },
};

beforeEach(() => {
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const method = init?.method ?? request?.method ?? "GET";
    const p = url.pathname;
    if (p === "/api/server")
      return json({
        version: "2.2.0",
        home: "",
        pid: 1,
        started_at: 0,
        url: "",
        base_path: "",
        title: "cereyan",
        served_dir: null,
        engines: [],
        queued: 0,
        stream_seq: 0,
        auth: false,
        exposed: false,
        token_file: null,
        paused: null,
      });
    if (p === "/api/runs/compare") {
      expect(url.searchParams.get("ids")).toBe("1,2");
      return json(comparison);
    }
    if (p === "/api/flows") return json([]);
    return json({ error: `unmocked ${method} ${p}` }, 404);
  });
});

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

test("shows the changed parameters, the first divergence, and the new errors", async () => {
  mount("/runs/compare?ids=1,2");
  expect(await screen.findByTestId("compare-summary")).toHaveTextContent("1 parameter changed");
  const params = screen.getAllByTestId("param-diff");
  expect(params).toHaveLength(1);
  expect(params[0]).toHaveTextContent("day");
  expect(screen.getByTestId("first-divergence")).toHaveTextContent("check-0");
  expect(screen.getByTestId("first-divergence")).toHaveTextContent("first divergence");
  expect(screen.getByTestId("new-errors")).toHaveTextContent("bad day");
  expect(screen.getByTestId("compare-baseline")).toHaveTextContent("run-1");
  expect(screen.getByTestId("compare-compared")).toHaveTextContent("run-2");
});

test("without two ids it explains how to get here", async () => {
  mount("/runs/compare");
  expect(await screen.findByText(/Pick two runs/)).toBeInTheDocument();
});
