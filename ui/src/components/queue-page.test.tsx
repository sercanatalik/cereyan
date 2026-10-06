// The Queue page: its summary line, the Capacity card (stepper, limits, slots, queue
// depth), Up next with Ready to start and Starting later, and the explainer.
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
let inLine = true;
const patches: unknown[] = [];
const posts: string[] = [];

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function queue() {
  return {
    processors: { count, cap: 8, source: "default", load: 0.25, items: engines },
    in_line: !inLine
      ? []
      : [
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
  inLine = true;
  patches.length = 0;
  posts.length = 0;
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const method = init?.method ?? request?.method ?? "GET";
    const p = url.pathname;
    if (p === "/api/queue") return json(queue());
    if (p === "/api/metrics/history")
      return json({
        interval_secs: 5,
        samples: [
          { at: 1, queued: 0, running: 1, engines_busy: 1 },
          { at: 2, queued: 3, running: 1, engines_busy: 1 },
          { at: 3, queued: 2, running: 1, engines_busy: 1 },
        ],
      });
    if (p === "/api/runs/40")
      return json({
        id: 40,
        name: "humble-macaw",
        flow_name: "orders",
        project: "etl",
        state: { type: "Running" },
      });
    if (p === "/api/runs/40/tasks")
      return json([
        { id: 1, dynamic_key: "extract-0", state: { type: "Completed" } },
        { id: 2, dynamic_key: "transform-0", state: { type: "Running" } },
      ]);
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
  return client;
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

test("a continuous loop can be paused, and a paused one resumed, from Starting later", async () => {
  mount("/queue");
  fireEvent.click(await screen.findByRole("button", { name: "Pause the sync_inbox loop" }));
  await waitFor(() => expect(posts).toContain("/api/schedules/9/pause"));
  expect(screen.getAllByTestId("paused-loop-row")[0]).toHaveTextContent("paused after failures");
  fireEvent.click(screen.getByRole("button", { name: "Resume the scrape_prices loop" }));
  await waitFor(() => expect(posts).toContain("/api/schedules/11/resume"));
  // A schedule's own run gets no loop control.
  expect(screen.queryByRole("button", { name: "Pause the report_daily loop" })).toBeNull();
});

test("the page opens with how many runs execute and wait", async () => {
  mount("/queue");
  expect(await screen.findByTestId("queue-summary")).toHaveTextContent(
    "1 run is executing. 2 runs are waiting in line.",
  );
});

test("the Capacity card shows busy of total, CPU, and the queue-depth sparkline", async () => {
  mount("/queue");
  const card = await screen.findByTestId("processors-card");
  expect(within(card).getByRole("heading", { name: "Capacity" })).toBeInTheDocument();
  expect(within(card).getByTestId("capacity-busy")).toHaveTextContent(
    "1 of 1 processor busy · machine CPU at 25%",
  );
  const spark = within(card).getByTestId("queue-sparkline");
  await waitFor(() =>
    expect(spark.querySelector("polyline")?.getAttribute("points")?.split(" ")).toHaveLength(3),
  );
  expect(spark).toHaveTextContent("2");
});

test("a running slot shows its run, flow and task; an idle one its module", async () => {
  count = 2;
  engines = [
    { id: "e1", status: "running", module: "etl.orders", run_id: 40, since_secs: 12 },
    { id: "e2", status: "idle", module: "ml.pipeline", run_id: null, since_secs: 300 },
  ];
  mount("/queue");
  const card = await screen.findByTestId("processors-card");
  const [running, idle] = await within(card).findAllByTestId("processor-tile");
  expect(running).toHaveTextContent("Running");
  expect(await within(running).findByText("humble-macaw")).toHaveAttribute("href", "/runs/40");
  await waitFor(() =>
    expect(within(running).getByTestId("slot-detail")).toHaveTextContent("etl/orders · task transform-0"),
  );
  expect(idle).toHaveTextContent("Idle");
  expect(idle).toHaveTextContent("Loaded: ml.pipeline");
});

test("removing a busy processor drains it with its run, and the slot goes once the run ends", async () => {
  count = 2;
  engines = [
    { id: "e1", status: "running", module: "etl.orders", run_id: 40, since_secs: 12 },
    { id: "e2", status: "running", module: "etl.orders", run_id: 42, since_secs: 5 },
  ];
  const client = mount("/queue");
  const card = await screen.findByTestId("processors-card");
  await within(card).findByText("Run #42");
  // The server answers the smaller count by draining the second engine.
  fetchMock.mockImplementationOnce(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const body = await (request ? request.clone().json() : JSON.parse(String(init?.body)));
    patches.push(body);
    count = body.max_engines;
    engines = [engines[0], { ...engines[1], status: "draining" }];
    return json({ max_engines: count, cpu_cap: 8, engine_saturation_risk: false, saturation_flows: [] });
  });
  fireEvent.click(within(card).getByRole("button", { name: "Remove a processor" }));
  await waitFor(() => expect(patches).toEqual([{ max_engines: 1 }]));
  await waitFor(() =>
    expect(
      within(card)
        .getAllByTestId("processor-tile")
        .map((t) => t.dataset.status),
    ).toEqual(["running", "draining"]),
  );
  expect(within(card).getByText("Run #42")).toBeInTheDocument();
  engines = [engines[0]];
  await client.invalidateQueries({ queryKey: ["queue"] });
  await waitFor(() => expect(within(card).getAllByTestId("processor-tile")).toHaveLength(1));
  expect(within(card).queryByText("Run #42")).toBeNull();
});

test("the saturation warning offers to add a processor", async () => {
  fetchMock.mockImplementation(
    ((orig) => async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = input instanceof Request ? input : null;
      const url = new URL(request ? request.url : String(input), "http://x");
      if (url.pathname === "/api/settings" && (init?.method ?? request?.method ?? "GET") === "GET")
        return json({
          max_engines: count,
          cpu_cap: 8,
          engine_saturation_risk: true,
          saturation_flows: ["etl/orders"],
        });
      return orig(input, init);
    })(
      fetchMock.getMockImplementation() as (
        input: RequestInfo | URL,
        init?: RequestInit,
      ) => Promise<Response>,
    ),
  );
  mount("/queue");
  const banner = await screen.findByTestId("saturation-banner");
  expect(banner).toHaveTextContent("These flows can occupy every processor: etl/orders.");
  fireEvent.click(within(banner).getByRole("button", { name: "Add a processor" }));
  await waitFor(() => expect(patches).toEqual([{ max_engines: 2 }]));
});

test("Up next splits runs ready to start from runs starting later", async () => {
  mount("/queue");
  const ready = await screen.findByRole("region", { name: "Ready to start" });
  expect(within(ready).getAllByTestId("in-line-row")).toHaveLength(2);
  expect(ready).toHaveTextContent("Ready to start2");
  const later = screen.getByRole("region", { name: "Starting later" });
  expect(later).toHaveTextContent("these hold no processor while they wait");
  expect(
    within(later)
      .getAllByTestId("joining-row")
      .map((r) => r.textContent),
  ).toEqual([expect.stringContaining("report_daily"), expect.stringContaining("sync_inbox")]);
});

test("with nothing in line, Ready to start says so and Starting later still lists runs not yet due", async () => {
  inLine = false;
  mount("/queue");
  const ready = await screen.findByRole("region", { name: "Ready to start" });
  expect(ready).toHaveTextContent("No run is waiting for a processor.");
  expect(
    within(screen.getByRole("region", { name: "Starting later" })).getAllByTestId("joining-row"),
  ).toHaveLength(2);
  expect(screen.getByTestId("queue-summary")).toHaveTextContent(
    "1 run is executing. Nothing is waiting, so new runs start at once.",
  );
});

test("How the queue works is closed by default and says removing a processor lets its run finish", async () => {
  mount("/queue");
  const explainer = await screen.findByTestId("queue-explainer");
  expect(explainer.tagName).toBe("DETAILS");
  expect(explainer).not.toHaveAttribute("open");
  expect(within(explainer).getByText("How the queue works").tagName).toBe("SUMMARY");
  expect(explainer).toHaveTextContent(
    "Removing a processor never interrupts a run: it finishes its current run",
  );
});
