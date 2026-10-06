// The dashboard: the quiet stat card, the Flows card with its log preview, the
// row of Upcoming, Needs attention and Running now, and Recently completed.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ProjectProvider } from "@/lib/project";
import { routeTree } from "@/routeTree.gen";

// The API client captures globalThis.fetch when it loads, so the stub must exist first.
const fetchMock = vi.hoisted(() => {
  const fn = vi.fn();
  globalThis.fetch = fn as unknown as typeof fetch;
  // Node's Request rejects the relative paths the client builds; resolve them.
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
const MINUTE = 60_000_000;

type Tuple = [number, string, string, number | null];

const run = (id: number, name: string, type: string, extra: Record<string, unknown> = {}) => ({
  id,
  external_id: `x${id}`,
  flow_id: 1,
  flow_name: "etl",
  project: "proj",
  name,
  parameters: {},
  tags: [],
  state: { type, name: type, message: null, details: {}, timestamp: NOW - MINUTE },
  created_at: NOW - 30 * MINUTE,
  start_time: NOW - 30 * MINUTE,
  end_time: type === "Running" ? null : NOW - MINUTE,
  total_run_time: type === "Running" ? null : 2_000_000,
  task_counts: {},
  created_by: "schedule",
  failure_count: 0,
  crash_count: 0,
  ...extra,
});

const flow = (id: number, name: string, recent: Tuple[], extra: Record<string, unknown> = {}) => ({
  id,
  external_id: `f${id}`,
  name,
  project: "proj",
  module: "pipeline",
  source_dir: "/p",
  tags: [],
  options: {},
  live: true,
  last_seen_at: NOW,
  created_at: 0,
  recent_runs: recent,
  schedules: [],
  triggers: [],
  ...extra,
});

let runs: ReturnType<typeof run>[];
let flows: ReturnType<typeof flow>[];
let upcoming: ReturnType<typeof run>[];
let counts: Record<string, number>;
let logs: Record<number, { id: number; level: number; message: string }[]>;
const calls: string[] = [];

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  setViewport(1440);
  calls.length = 0;
  runs = [run(1, "done-one", "Completed")];
  flows = [];
  upcoming = [];
  counts = {};
  logs = {};
  fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
    const url = new URL(input instanceof Request ? input.url : String(input), "http://x");
    const p = url.pathname;
    calls.push(p + url.search);
    if (p === "/api/counts") return json({ runs: counts, task_runs: {}, active: 0, flows: {} });
    if (p === "/api/flows") return json(flows);
    if (p === "/api/queue")
      return json({
        in_line: [],
        joining: [],
        more: 0,
        paused_loops: [],
        processors: { cap: 4, count: 4, hosts: [], items: [], source: "settings" },
      });
    if (p === "/api/runs") {
      if (url.searchParams.get("state_type") === "Scheduled")
        return json({ items: upcoming.slice(0, Number(url.searchParams.get("limit"))), next_cursor: null });
      return json({ items: runs, next_cursor: null });
    }
    const logMatch = /^\/api\/runs\/(\d+)\/logs$/.exec(p);
    if (logMatch) {
      const id = Number(logMatch[1]);
      const items = (logs[id] ?? []).map((l) => ({
        ...l,
        run_id: id,
        logger: "cereyan",
        timestamp: NOW - MINUTE,
      }));
      return json({ items, next_cursor: null });
    }
    const runMatch = /^\/api\/runs\/(\d+)$/.exec(p);
    if (runMatch) {
      const id = Number(runMatch[1]);
      return json(run(id, `run-${id}`, "Failed", { flow_name: "flaky_load" }));
    }
    return json({ error: `unmocked ${p}` }, 404);
  });
});
afterEach(() => fetchMock.mockReset());

/** Answer `(min-width: Npx)` queries as a viewport `px` wide would. */
function setViewport(px: number) {
  window.matchMedia = ((query: string) => {
    const m = /min-width:\s*(\d+)px/.exec(query);
    return {
      matches: m ? px >= Number(m[1]) : false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    };
  }) as unknown as typeof window.matchMedia;
}

function mount() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: ["/"] }),
    context: { queryClient: client },
  });
  render(
    <QueryClientProvider client={client}>
      <ProjectProvider>
        <RouterProvider router={router} />
      </ProjectProvider>
    </QueryClientProvider>,
  );
  return { client, router };
}

const logCalls = () => calls.filter((c) => /^\/api\/runs\/\d+\/logs/.test(c)).map((c) => c.split("/")[3]);
const squares = (flowName: string) =>
  within(
    within(document.querySelector(`[data-flow="${flowName}"]`) as HTMLElement).getByTestId("run-strip"),
  ).getAllByRole("button");

test("zero counts stay quiet: four counts and the total, no proportion bar or queue sparkline", async () => {
  counts = { Paused: 0, Scheduled: 2 };
  mount();
  const card = await screen.findByTestId("stat-card");
  await waitFor(() => expect(screen.getByTestId("count-total")).toHaveTextContent("1"));
  const shown = [...card.querySelectorAll("[data-testid^='count-']")].map((e) =>
    e.getAttribute("data-testid"),
  );
  expect(shown).toEqual([
    "count-Completed",
    "count-Failed",
    "count-Paused",
    "count-Scheduled",
    "count-total",
  ]);
  expect(screen.getByTestId("count-Paused")).toHaveTextContent("Waiting for input");
  expect(within(card).queryByTestId("state-bar")).toBeNull();
  expect(screen.queryByTestId("queue-sparkline")).toBeNull();
  expect(calls.some((c) => c.startsWith("/api/metrics/history"))).toBe(false);
  expect(within(card).getByRole("img", { name: "Run activity" })).toBeInTheDocument();
});

test("a crash appears in the stat card within one refetch", async () => {
  const { client } = mount();
  await screen.findByTestId("count-total");
  expect(screen.queryByTestId("count-Crashed")).toBeNull();
  runs = [...runs, run(2, "broke-down", "Crashed")];
  await act(() => client.refetchQueries({ queryKey: ["runs", "dashboard"] }));
  expect(await screen.findByTestId("count-Crashed")).toHaveTextContent("1");
});

test("switching the range refetches counts and lists without leaving the page", async () => {
  const { router } = mount();
  await screen.findByTestId("count-total");
  const before = calls.filter((c) => c.startsWith("/api/runs?") && c.includes("start_after")).length;
  fireEvent.click(screen.getByRole("button", { name: /Date range/ }));
  fireEvent.click(await screen.findByText("Past 7 days"));
  await waitFor(() =>
    expect(
      calls.filter((c) => c.startsWith("/api/runs?") && c.includes("start_after")).length,
    ).toBeGreaterThan(before),
  );
  expect(router.state.location.pathname).toBe("/");
});

test("nothing running: a centred empty state with the idle processor count", async () => {
  mount();
  const empty = await screen.findByTestId("running-empty");
  expect(empty).toHaveTextContent("Nothing running");
  await waitFor(() => expect(empty).toHaveTextContent("4 processors idle"));
  expect(empty.className).toContain("justify-center");
  expect(empty.className).toContain("flex-1");
  expect(screen.getByTestId("dashboard-lists").className).toContain("items-stretch");
});

test("progress updates live: a running run's task bar advances within one refetch", async () => {
  runs = [run(5, "busy-bee", "Running", { task_counts: { Completed: 1, Running: 1, Pending: 2 } })];
  counts = { Running: 1 };
  const { client } = mount();
  expect(await screen.findByTestId("task-progress")).toHaveTextContent("1 of 4 tasks");
  runs = [run(5, "busy-bee", "Running", { task_counts: { Completed: 2, Running: 1, Pending: 1 } })];
  await act(() => client.refetchQueries({ queryKey: ["runs", "dashboard"] }));
  await waitFor(() => expect(screen.getByTestId("task-progress")).toHaveTextContent("2 of 4 tasks"));
});

test("Upcoming stays small: three rows, a time column, and a link to all of them", async () => {
  counts = { Scheduled: 7 };
  upcoming = Array.from({ length: 7 }, (_, i) =>
    run(100 + i, `soon-${i}`, "Scheduled", {
      flow_name: i === 0 ? "etl" : `flow-${i}`,
      scheduled_time: NOW + (3 + i * 10) * MINUTE + 10_000_000,
    }),
  );
  mount();
  const card = await screen.findByTestId("upcoming-card");
  await waitFor(() => expect(within(card).getAllByTestId("upcoming-row")).toHaveLength(3));
  const first = within(card).getAllByTestId("upcoming-row")[0];
  expect(first).toHaveTextContent("in 3 min");
  expect(first).toHaveTextContent("etl");
  expect(first).toHaveTextContent("schedule");
  expect(first.firstElementChild?.className).toContain("font-mono");
  expect(within(card).getByRole("link", { name: "All 7" })).toHaveAttribute("href", "/queue");
  expect(calls.some((c) => c.includes("state_type=Scheduled") && c.includes("limit=3"))).toBe(true);
});

test("Upcoming reads Nothing scheduled. when none are due", async () => {
  mount();
  const card = await screen.findByTestId("upcoming-card");
  expect(await within(card).findByText("Nothing scheduled.")).toBeInTheDocument();
});

test("Needs attention shows five one-line rows with one action each, then Show all", async () => {
  runs = [
    run(1, "waits", "Paused", { state: { type: "Paused", name: "Paused", details: { prompt: "Go?" } } }),
    ...Array.from({ length: 5 }, (_, i) =>
      run(10 + i, `bad-${i}`, "Failed", {
        state: { type: "Failed", name: "Failed", message: `ValueError ${i}`, timestamp: NOW },
      }),
    ),
    run(20, "fell-over", "Crashed"),
  ];
  mount();
  const card = await screen.findByTestId("attention-card");
  await waitFor(() => expect(within(card).getAllByTestId("attention-row")).toHaveLength(5));
  expect(within(card).getByText("7")).toBeInTheDocument();
  const [paused, failed] = within(card).getAllByTestId("attention-row");
  expect(paused).toHaveTextContent("waits is waiting for input");
  expect(paused).toHaveTextContent("Go?");
  expect(
    within(paused)
      .getAllByRole("button")
      .map((b) => b.textContent?.trim()),
  ).toEqual(["Answer"]);
  expect(failed).toHaveTextContent("bad-0 failed");
  expect(
    within(failed)
      .getAllByRole("button")
      .map((b) => b.textContent?.trim()),
  ).toEqual(["Run again"]);
  expect(within(card).getByRole("link", { name: "Show all 7" })).toHaveAttribute("href", "/runs?tab=runs");
});

test("a crashed run offers Run again, and a single state's Show all filters the Runs page", async () => {
  runs = Array.from({ length: 6 }, (_, i) => run(30 + i, `crash-${i}`, "Crashed"));
  mount();
  const card = await screen.findByTestId("attention-card");
  const row = (await within(card).findAllByTestId("attention-row"))[0];
  expect(row).toHaveTextContent("crashed");
  expect(within(row).getByRole("button")).toHaveTextContent("Run again");
  expect(within(card).getByRole("link", { name: "Show all 6" })).toHaveAttribute(
    "href",
    "/runs?tab=runs&state=Crashed",
  );
});

test("a completed run appears first in Recently completed within one refetch, still at most eight", async () => {
  runs = Array.from({ length: 8 }, (_, i) =>
    run(i + 1, `done-${i + 1}`, "Completed", { end_time: NOW - (i + 2) * MINUTE }),
  );
  const { client } = mount();
  const card = await screen.findByTestId("recently-completed");
  await waitFor(() => expect(within(card).getAllByTestId("completed-name")).toHaveLength(8));
  runs = [...runs, run(50, "fresh-one", "Completed", { end_time: NOW })];
  await act(() => client.refetchQueries({ queryKey: ["runs", "dashboard"] }));
  await waitFor(() =>
    expect(within(card).getAllByTestId("completed-name")[0]).toHaveTextContent("fresh-one"),
  );
  expect(within(card).getAllByTestId("completed-name")).toHaveLength(8);
});

test("the Flows card lists failing flows first, leaves out never-run ones, and stops at eight", async () => {
  flows = [
    flow(1, "report", [[40, "Completed", "Completed", 1_000_000]]),
    flow(2, "flaky_load", [[30, "Failed", "Failed", 41_000]], {
      schedules: [{ active: true, next_fire: NOW + 2 * 86_400_000_000 }],
    }),
    flow(3, "render", []),
    flow(4, "approve", [[35, "Paused", "Paused", null]]),
    ...Array.from({ length: 8 }, (_, i) =>
      flow(10 + i, `quiet-${i}`, [[i + 1, "Completed", "Completed", 1_000]] as Tuple[]),
    ),
  ];
  mount();
  await screen.findAllByTestId("dashboard-flow");
  const names = screen.getAllByTestId("dashboard-flow").map((r) => r.getAttribute("data-flow"));
  expect(names).toHaveLength(8);
  expect(names.slice(0, 3)).toEqual(["flaky_load", "approve", "report"]);
  expect(names).not.toContain("render");
  const status = within(document.querySelector('[data-flow="flaky_load"]') as HTMLElement).getByTestId(
    "flow-status",
  );
  expect(status.textContent).toMatch(/^Failed · 41 ms · next /);
  expect(status.className).toContain("text-red-700");
  expect(screen.getByRole("link", { name: "All 12 flows" })).toHaveAttribute("href", "/flows");
});

test("on load the newest failed run is selected and its logs shown with errors highlighted", async () => {
  flows = [
    flow(1, "flaky_load", [
      [30, "Failed", "Failed", 41_000],
      [20, "Failed", "Failed", 41_000],
    ]),
    flow(2, "report", [[25, "Completed", "Completed", 1_000]]),
  ];
  logs = {
    30: [
      { id: 1, level: 20, message: "loading rows" },
      { id: 2, level: 30, message: "slow source" },
      { id: 3, level: 40, message: "ValueError: too many rows" },
    ],
  };
  mount();
  const preview = await screen.findByTestId("log-preview");
  expect(preview).toHaveAttribute("data-run-id", "30");
  expect(await within(preview).findByText("run-30")).toBeInTheDocument();
  const error = (await within(preview).findByText("ValueError: too many rows")).closest("[data-level]");
  expect(error).toHaveAttribute("data-level", "ERROR");
  expect(error?.className).toContain("bg-destructive");
  expect(within(preview).getByText("slow source").closest("[data-level]")?.className).toContain("bg-amber");
  expect(within(preview).getByTestId("log-preview-lines").className).toContain("max-h-60");
  expect(within(preview).getByTestId("log-preview-lines").className).toContain("overflow-auto");
  const strip = squares("flaky_load");
  expect(strip.at(-1)).toHaveAttribute("aria-pressed", "true");
  // Only the selected run's logs are fetched.
  expect(new Set(logCalls())).toEqual(new Set(["30"]));
  expect(within(preview).getByRole("link", { name: "Open run" })).toHaveAttribute(
    "href",
    "/runs/30?tab=logs",
  );
});

test("with no failure nothing is selected, a hint shows, and no logs are fetched", async () => {
  flows = [flow(1, "report", [[25, "Completed", "Completed", 1_000]])];
  mount();
  expect(await screen.findByText("Click a run square to see its logs here.")).toBeInTheDocument();
  expect(screen.queryByTestId("log-preview")).toBeNull();
  expect(logCalls()).toEqual([]);
});

test("a square loads that run's logs, the same square again closes it, and so does the close button", async () => {
  flows = [
    flow(1, "etl", [
      [9, "Completed", "Completed", 1_000],
      [8, "Completed", "Completed", 1_000],
      [7, "Completed", "Completed", 1_000],
      [6, "Completed", "Completed", 1_000],
      [5, "Completed", "Completed", 1_000],
    ]),
  ];
  logs = { 8: [{ id: 1, level: 20, message: "hello from eight" }] };
  mount();
  await screen.findByText("Click a run square to see its logs here.");
  // Five neutral slots, then runs 5..9 oldest to newest: the fourth run square is run 8.
  const fourth = squares("etl")[3];
  fireEvent.click(fourth);
  const preview = await screen.findByTestId("log-preview");
  expect(preview).toHaveAttribute("data-run-id", "8");
  expect(await within(preview).findByText("hello from eight")).toBeInTheDocument();
  expect(fourth).toHaveAttribute("aria-pressed", "true");
  fireEvent.click(fourth);
  await waitFor(() => expect(screen.queryByTestId("log-preview")).toBeNull());
  expect(squares("etl").every((b) => b.getAttribute("aria-pressed") === "false")).toBe(true);
  fireEvent.click(squares("etl")[0]);
  fireEvent.click(await screen.findByRole("button", { name: "Close logs" }));
  await waitFor(() => expect(screen.queryByTestId("log-preview")).toBeNull());
  expect(new Set(logCalls())).toEqual(new Set(["8", "5"]));
});

test("a name in Recently completed opens its preview without navigating", async () => {
  flows = [flow(1, "etl", [[1, "Completed", "Completed", 1_000]])];
  runs = [run(1, "humble-macaw", "Completed")];
  logs = { 1: [{ id: 1, level: 20, message: "macaw says hi" }] };
  const { router } = mount();
  const card = await screen.findByTestId("recently-completed");
  fireEvent.click(await within(card).findByRole("button", { name: "humble-macaw" }));
  const preview = await screen.findByTestId("log-preview");
  expect(preview).toHaveAttribute("data-run-id", "1");
  expect(await within(preview).findByText("macaw says hi")).toBeInTheDocument();
  expect(router.state.location.pathname).toBe("/");
});
