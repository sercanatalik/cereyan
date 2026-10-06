// The run detail page: header band, facts, error card, tasks rail, passes and tab counts.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { TaskRail } from "@/components/task-rail";
import { ProjectProvider } from "@/lib/project";
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
const state = (type: string, extra: Record<string, unknown> = {}) => ({
  type,
  name: type,
  message: null,
  details: {},
  timestamp: NOW - 60_000_000,
  ...extra,
});
const run = (id: number, st: any, extra: Record<string, unknown> = {}) => ({
  id,
  external_id: `x${id}`,
  flow_id: 3,
  flow_name: "flaky_load",
  project: "examples",
  name: `run-${id}`,
  parameters: { rows: 5 },
  tags: [],
  state: st,
  failure_count: 0,
  crash_count: 0,
  created_at: NOW - 120_000_000,
  start_time: NOW - 100_000_000,
  end_time: NOW - 99_959_000,
  total_run_time: 41_000,
  created_by: "api",
  attempt: 0,
  priority: 0,
  host: "server",
  processor: 2,
  ...extra,
});
const task = (id: number, runId: number, key: string, type: string, extra: Record<string, unknown> = {}) => ({
  id,
  external_id: `t${id}`,
  run_id: runId,
  name: key.replace(/-\d+$/, ""),
  task_key: key.replace(/-\d+$/, ""),
  dynamic_key: key,
  state: state(type),
  failure_count: 0,
  crash_count: 0,
  created_at: NOW - 100_000_000,
  start_time: NOW - 100_000_000,
  end_time: NOW - 99_990_000 + id,
  total_run_time: 1_000,
  pass: 0,
  parents: [],
  ...extra,
});

const RUNS: Record<number, any> = {
  // Failed on its second attempt, at load-0.
  1: run(1, state("Failed", { message: "ValueError: too many rows" }), { attempt: 1, parent_run_id: 9 }),
  2: run(2, state("Running"), { end_time: null, total_run_time: null }),
  3: run(3, state("Completed"), { total_run_time: 26_000, schedule_id: 4, created_by: "schedule" }),
  // Paused and resumed: pass 0 then pass 1.
  4: run(4, state("Completed")),
};
const TASKS: Record<number, any[]> = {
  1: [
    task(10, 1, "extract-0", "Completed"),
    task(11, 1, "transform-0", "Completed"),
    task(12, 1, "load-0", "Failed"),
  ],
  2: [task(20, 2, "extract-0", "Running", { end_time: null, total_run_time: null })],
  3: [task(30, 3, "extract-0", "Completed"), task(31, 3, "load-0", "Completed")],
  4: [
    task(40, 4, "ask-0", "Completed", { pass: 0 }),
    task(41, 4, "ship-0", "Completed", { pass: 1 }),
    task(42, 4, "notify-0", "Completed", { pass: 1 }),
  ],
};
const log = (id: number, level: number, message: string, task_run_id: number | null = null) => ({
  id,
  run_id: 1,
  task_run_id,
  timestamp: NOW - 99_990_000 + id * 1000,
  level,
  logger: "flow",
  message,
});
const LOGS = [
  log(1, 20, "run started"),
  log(2, 20, "extracting", 10),
  log(3, 20, "loading", 12),
  log(4, 40, "ValueError: too many rows", 12),
  log(5, 40, "run failed"),
];

const calls: { method: string; url: string; body?: any }[] = [];
function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}
beforeEach(() => {
  calls.length = 0;
  vi.spyOn(window, "confirm").mockReturnValue(true);
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const req = input instanceof Request ? input : null;
    const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url);
    const method = init?.method ?? req?.method ?? "GET";
    const text = req && method !== "GET" ? await req.clone().text() : undefined;
    calls.push({ method, url: url.pathname + url.search, body: text ? JSON.parse(text) : undefined });
    const p = url.pathname;
    const id = Number(p.split("/")[3]);
    if (p === "/api/runs") return json({ items: [], next_cursor: null });
    if (p === "/api/flows" || p === "/api/counts" || p === "/api/settings")
      return json(p === "/api/flows" ? [] : {});
    if (/^\/api\/runs\/\d+$/.test(p))
      return RUNS[id] ? json(RUNS[id]) : json({ error: "run not found" }, 404);
    if (/^\/api\/runs\/\d+\/tasks$/.test(p)) return json(TASKS[id] ?? []);
    if (/^\/api\/runs\/\d+\/logs$/.test(p)) return json({ items: id === 1 ? LOGS : [], next_cursor: null });
    if (/^\/api\/task-runs\/\d+\/logs$/.test(p))
      return json({ items: LOGS.filter((l) => l.task_run_id === id), next_cursor: null });
    if (/^\/api\/runs\/\d+\/graph$/.test(p)) return json({ nodes: [], edges: [] });
    if (/^\/api\/runs\/\d+\/state$/.test(p)) return json([]);
    if (/^\/api\/runs\/\d+\/cancel$/.test(p)) return json({ ...RUNS[id], state: state("Cancelling") });
    if (/^\/api\/runs\/\d+\/retry$/.test(p)) return json({ run: { ...RUNS[1], id: 50 } });
    if (/^\/api\/flows\/\d+\/runs$/.test(p)) return json({ ...RUNS[3], id: 60 });
    if (p === "/api/artifacts")
      return json({ items: url.searchParams.get("run_id") === "1" ? [{ id: 1 }] : [], next_cursor: null });
    return json({ error: `unmocked ${method} ${p}` }, 404);
  });
});
afterEach(() => {
  fetchMock.mockReset();
  vi.restoreAllMocks();
  vi.useRealTimers();
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
  return { client, router };
}

const header = () => screen.findByTestId("run-header");
const button = (name: string) => within(screen.getByTestId("run-header")).queryByRole("button", { name });

test("header: summary sentence, facts grid, flow in the crumb, actions for a failed run", async () => {
  mount("/runs/1");
  await header();
  await waitFor(() =>
    expect(screen.getByTestId("run-summary")).toHaveTextContent(
      "Failed after 41 ms on the 2nd attempt. 2 of 3 tasks completed.",
    ),
  );
  const facts = screen.getByTestId("run-facts");
  for (const label of ["Started", "Duration", "Attempts", "Triggered by", "Ran on", "Parameters"])
    expect(within(facts).getByText(label)).toBeInTheDocument();
  expect(facts).toHaveTextContent("Attempts2");
  expect(facts).toHaveTextContent("rows=5");
  expect(within(facts).getByRole("link", { name: "retry of run 9" })).toHaveAttribute("href", "/runs/9");
  expect(within(facts).getByTestId("run-host")).toHaveTextContent("server · 2");
  expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent("examples/flaky_load");
  expect(button("Retry from failure")).toBeInTheDocument();
  expect(button("Run again")).toBeInTheDocument();
  expect(button("Cancel")).toBeNull();
});

test("Run again creates a run with the same parameters and opens it", async () => {
  const { router } = mount("/runs/3");
  await header();
  expect(button("Retry from failure")).toBeNull();
  fireEvent.click(button("Run again") as HTMLElement);
  await waitFor(() => expect(router.state.location.pathname).toBe("/runs/60"));
  const post = calls.find((c) => c.method === "POST" && c.url === "/api/flows/3/runs");
  expect(post?.body.parameters).toEqual({ rows: 5 });
});

test("Retry from failure calls retry and opens the new run", async () => {
  const { router } = mount("/runs/1");
  await header();
  fireEvent.click(button("Retry from failure") as HTMLElement);
  await waitFor(() => expect(router.state.location.pathname).toBe("/runs/50"));
  expect(calls.find((c) => c.url === "/api/runs/1/retry")?.body).toEqual({ from: "failure" });
});

test("Cancel on a running run shows Cancelling and then Cancelled", async () => {
  const { client } = mount("/runs/2");
  await header();
  expect(screen.getByTestId("run-facts")).toHaveTextContent("Elapsed");
  fireEvent.click(button("Cancel") as HTMLElement);
  expect(window.confirm).toHaveBeenCalled();
  await waitFor(() =>
    expect(within(screen.getByTestId("run-header")).getByText("Cancelling")).toBeInTheDocument(),
  );
  // The live stream then reports the run Cancelled.
  act(() => client.setQueryData(["run", 2], { ...RUNS[2], state: state("Cancelled") }));
  await waitFor(() =>
    expect(within(screen.getByTestId("run-header")).getByText("Cancelled")).toBeInTheDocument(),
  );
  expect(button("Cancel")).toBeNull();
  expect(button("Retry from failure")).toBeInTheDocument();
});

test("a Completed run has no Cancel, and Delete sits in the overflow menu", async () => {
  mount("/runs/3");
  await header();
  expect(screen.getByTestId("run-summary")).toHaveTextContent("Completed in 26 ms. 2 of 2 tasks completed.");
  expect(button("Cancel")).toBeNull();
  expect(screen.queryByTestId("error-card")).toBeNull();
  expect(within(screen.getByTestId("run-facts")).getByRole("link", { name: "schedule" })).toHaveAttribute(
    "href",
    "/flows/3",
  );
  fireEvent.keyDown(button("More actions") as HTMLElement, { key: "Enter" });
  expect(await screen.findByRole("menuitem", { name: "Delete run" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Cancel run" })).toBeNull();
});

test("error card names the failing task and Show in logs selects it and scrolls to its first ERROR", async () => {
  const scrolled: Element[] = [];
  vi.spyOn(Element.prototype, "scrollIntoView").mockImplementation(function (this: Element) {
    scrolled.push(this);
  });
  const { router } = mount("/runs/1");
  const card = await screen.findByTestId("error-card");
  await waitFor(() => expect(card).toHaveTextContent("load-0"));
  expect(card).toHaveTextContent("ValueError: too many rows");
  fireEvent.click(within(card).getByRole("button", { name: "Show in logs" }));
  await waitFor(() => expect(router.state.location.search).toMatchObject({ task: 12 }));
  expect(await screen.findByTestId("log-task-chip")).toHaveTextContent("load-0");
  await waitFor(() => {
    const target = scrolled.find((el) => el.hasAttribute("data-first-error"));
    expect(target).toHaveTextContent("ValueError: too many rows");
  });
  expect(screen.getByRole("switch", { name: "Follow" })).not.toBeChecked();
});

test("tab labels show counts and log lines name their task", async () => {
  mount("/runs/1");
  await header();
  const tabs = screen.getByRole("tablist");
  await waitFor(() => expect(within(tabs).getByRole("tab", { name: /Logs/ })).toHaveTextContent("Logs5"));
  await waitFor(() =>
    expect(within(tabs).getByRole("tab", { name: /Artifacts/ })).toHaveTextContent("Artifacts1"),
  );
  expect(within(tabs).getByRole("tab", { name: /Parameters/ })).toHaveTextContent("Parameters1");
  expect(within(tabs).getByRole("tab", { name: /Timeline/ })).toHaveTextContent("Timeline3");
  expect(within(tabs).getByRole("tab", { name: "Details" })).toBeInTheDocument();
  const list = screen.getByTestId("log-list");
  await waitFor(() => expect(list).toHaveTextContent("extracting"));
  expect(within(list).getAllByText("extract-0").length).toBeGreaterThan(0);
});

test("a run with one pass has no pass selector and lists that pass", async () => {
  mount("/runs/1");
  await header();
  await screen.findByRole("option", { name: /load-0/ });
  expect(screen.queryByTestId("pass-switcher")).toBeNull();
  expect(screen.getAllByRole("option")).toHaveLength(3);
});

test("switching passes shows that pass in the rail and the Timeline, latest first", async () => {
  mount("/runs/4");
  await screen.findByRole("option", { name: /ship-0/ });
  const switcher = screen.getByTestId("pass-switcher");
  expect(screen.getAllByRole("option").map((o) => o.getAttribute("data-task-id"))).toEqual(["41", "42"]);
  fireEvent.click(within(switcher).getByRole("button", { name: "0" }));
  await screen.findByRole("option", { name: /ask-0/ });
  expect(screen.queryByRole("option", { name: /ship-0/ })).toBeNull();
  fireEvent.click(screen.getByRole("tab", { name: /Timeline/ }));
  await waitFor(() => expect(calls.some((c) => c.url === "/api/runs/4/graph?pass=0")).toBe(true));
});

test("an AwaitingRetry task counts down to its next attempt", () => {
  vi.useFakeTimers();
  const now = Date.now();
  const waiting = task(70, 7, "validate-0", "Scheduled", {
    state: {
      ...state("Scheduled"),
      name: "AwaitingRetry",
      timestamp: now * 1000,
      details: { attempt: 2, delay: 24, retries: 3 },
    },
  });
  render(<TaskRail tasks={[waiting] as any} onSelect={() => {}} />);
  expect(screen.getByTestId("retry-line")).toHaveTextContent("retry 2 of 3 · in 24s");
  expect(screen.getByTestId("task-state")).toHaveTextContent("Awaiting retry");
  act(() => vi.advanceTimersByTime(1000));
  expect(screen.getByTestId("retry-line")).toHaveTextContent("retry 2 of 3 · in 23s");
});
