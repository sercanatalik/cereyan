// The Queue page: the processor stepper, its limits, draining tiles, and why runs wait.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ProjectProvider } from "@/lib/project";
import { reasonText } from "@/routes/queue";
import { routeTree } from "@/routeTree.gen";

// The API client captures globalThis.fetch when it loads, so the stub must exist first.
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
let count = 1;
let engines: { id: string; status: string; module: string; run_id: number | null; since_secs: number }[] = [];
const patches: unknown[] = [];
const posts: string[] = [];

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function queue() {
  return {
    processors: { count, cap: 8, source: "default", load: 0.25, items: engines },
    in_line: [
      {
        run_id: 41,
        run_name: "a",
        flow: "train_model",
        project: "ml",
        module: "ml.pipeline",
        trigger: "API",
        priority: 5,
        position: 1,
        waited_us: 238_000_000,
        can_start: false,
        reason: "resource:gpu",
        overtaken_by: 2,
      },
      {
        run_id: 43,
        run_name: "b",
        flow: "ingest_orders",
        project: "etl",
        module: "etl.orders",
        trigger: "rule",
        priority: 0,
        position: 2,
        waited_us: 30_000_000,
        can_start: true,
        reason: null,
        overtaken_by: 0,
      },
    ],
    more: 0,
    joining: [
      {
        run_id: 50,
        run_name: "c",
        flow: "report_daily",
        project: "r",
        kind: "schedule",
        schedule_id: 3,
        at: NOW + 600_000_000,
      },
      {
        run_id: 51,
        run_name: "d",
        flow: "sync_inbox",
        project: "mail",
        kind: "continuous",
        schedule_id: 9,
        at: NOW + 1_380_000_000,
      },
    ],
    paused_loops: [
      {
        schedule_id: 11,
        flow: "scrape_prices",
        project: "web",
        reason: "disabled",
        until: NOW + 3_600_000_000,
      },
    ],
  };
}

beforeEach(() => {
  count = 1;
  engines = [{ id: "e1", status: "running", module: "etl.orders", run_id: 40, since_secs: 65 }];
  patches.length = 0;
  posts.length = 0;
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const method = init?.method ?? request?.method ?? "GET";
    const p = url.pathname;
    if (p === "/api/queue") return json(queue());
    if (/^\/api\/schedules\/\d+\/(pause|resume)$/.test(p) && method === "POST") {
      posts.push(p);
      return json({});
    }
    if (p === "/api/settings" && method === "PATCH") {
      const body = await (request ? request.clone().json() : JSON.parse(String(init?.body)));
      patches.push(body);
      count = body.max_engines;
      return json({ max_engines: count, cpu_cap: 8, engine_saturation_risk: false, saturation_flows: [] });
    }
    if (p === "/api/settings")
      return json({ max_engines: count, cpu_cap: 8, engine_saturation_risk: false, saturation_flows: [] });
    if (p === "/api/server")
      return json({ title: "cereyan", queued: 2, engines: [], paused: null, exposed: false, auth: false });
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

test("reason text for each way a run waits", () => {
  expect(reasonText({ can_start: true, reason: null, position: 1 })).toBe("Next");
  expect(reasonText({ can_start: true, reason: null, position: 3 })).toBe("Ready");
  expect(reasonText({ can_start: false, reason: "resource:gpu", position: 1 })).toBe("Waiting for gpu");
  expect(reasonText({ can_start: false, reason: "max_concurrent", position: 2 })).toBe("At max_concurrent");
  expect(reasonText({ can_start: false, reason: "no processor", position: 2 })).toBe("No free processor");
});

test("a waiting run says why and how many went ahead", async () => {
  mount("/queue");
  const rows = await screen.findAllByTestId("in-line-row");
  expect(rows[0]).toHaveTextContent("Waiting for gpu");
  expect(rows[0]).toHaveTextContent("2 runs went ahead");
  expect(rows[1]).toHaveTextContent("Ready");
  expect(screen.getByText("report_daily")).toBeInTheDocument();
});

test("the Queue tab carries the depth badge", async () => {
  mount("/queue");
  const nav = await screen.findByRole("navigation", { name: "Sections" });
  expect(await within(nav).findByTestId("queue-badge")).toHaveTextContent("2");
});

test("adding a processor patches max_engines and remove is disabled at one", async () => {
  mount("/queue");
  const card = await screen.findByTestId("processors-card");
  expect(within(card).getByTestId("processor-count")).toHaveTextContent("1 / 8 CPUs");
  expect(within(card).getByRole("button", { name: "Remove a processor" })).toBeDisabled();
  fireEvent.click(within(card).getByRole("button", { name: "Add a processor" }));
  await waitFor(() => expect(patches).toEqual([{ max_engines: 2 }]));
  await waitFor(() => expect(within(card).getByTestId("processor-count")).toHaveTextContent("2 / 8 CPUs"));
  expect(
    within(card)
      .getAllByTestId("processor-tile")
      .map((t) => t.dataset.status),
  ).toEqual(["running", "available"]);
});

test("add is disabled at the CPU cap", async () => {
  count = 8;
  mount("/queue");
  const card = await screen.findByTestId("processors-card");
  expect(
    await within(card).findByRole("button", { name: "Add a processor (at the 8 CPU cap)" }),
  ).toBeDisabled();
});

test("a draining processor keeps its run on a tile of its own", async () => {
  count = 1;
  engines = [
    { id: "e1", status: "running", module: "etl.orders", run_id: 40, since_secs: 65 },
    { id: "e2", status: "draining", module: "etl.orders", run_id: 42, since_secs: 12 },
  ];
  mount("/queue");
  const card = await screen.findByTestId("processors-card");
  await waitFor(() =>
    expect(
      within(card)
        .getAllByTestId("processor-tile")
        .map((t) => t.dataset.status),
    ).toEqual(["running", "draining"]),
  );
  expect(within(card).getByText("Run #42")).toBeInTheDocument();
  expect(card).toHaveTextContent("1 draining");
});

test("a continuous loop can be paused, and a paused one resumed, from Joining the line", async () => {
  mount("/queue");
  fireEvent.click(await screen.findByRole("button", { name: "Pause the sync_inbox loop" }));
  await waitFor(() => expect(posts).toContain("/api/schedules/9/pause"));
  expect(screen.getAllByTestId("paused-loop-row")[0]).toHaveTextContent("paused after failures");
  fireEvent.click(screen.getByRole("button", { name: "Resume the scrape_prices loop" }));
  await waitFor(() => expect(posts).toContain("/api/schedules/11/resume"));
  // A schedule's own run gets no loop control.
  expect(screen.queryByRole("button", { name: "Pause the report_daily loop" })).toBeNull();
});
