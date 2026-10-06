// The console shell, the project scope, the palette, and the four rebuilt pages.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ProjectProvider, useProject } from "@/lib/project";
import { routeTree } from "@/routeTree.gen";
import { FilterSelect } from "./filter-select";
import { StateBadge } from "./state-badge";
import { retryLine, TaskRail } from "./task-rail";

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
const state = (type: string, name = type, extra: Record<string, unknown> = {}) => ({
  type,
  name,
  message: null,
  details: {},
  timestamp: NOW - 60_000_000,
  ...extra,
});
const run = (
  id: number,
  name: string,
  flow: string,
  project: string,
  st: any,
  extra: Record<string, unknown> = {},
) => ({
  id,
  external_id: `x${id}`,
  flow_id: flow === "daily_orders" ? 1 : flow === "customer_dim" ? 2 : 3,
  flow_name: flow,
  project,
  name,
  parameters: { day: "2026-09-05" },
  tags: ["daily"],
  state: st,
  failure_count: 0,
  crash_count: 0,
  created_at: NOW - 120_000_000,
  start_time: NOW - 100_000_000,
  end_time: null,
  total_run_time: null,
  engine_pid: null,
  engine_id: null,
  created_by: "schedule",
  report_seq: 0,
  attempt: 1,
  priority: 0,
  task_counts: {},
  ...extra,
});
const RUNS = [
  run(10, "brisk-otter", "daily_orders", "warehouse", state("Running"), {
    task_counts: { Completed: 7, Running: 1, Pending: 4 },
  }),
  run(
    11,
    "quiet-heron",
    "customer_dim",
    "warehouse",
    state("Paused", "Paused", { details: { prompt: "Approve 1,204 deletes?" } }),
  ),
  run(
    12,
    "lucky-finch",
    "stripe_sync",
    "ingest",
    state("Failed", "Failed", { message: "KeyError: 'balance_transaction'" }),
    {
      total_run_time: 4_100_000,
    },
  ),
  run(13, "amber-wombat", "daily_orders", "warehouse", state("Scheduled", "Late"), {
    scheduled_time: NOW - 1_000_000,
  }),
];
const flow = (id: number, name: string, project: string, extra: Record<string, unknown> = {}) => ({
  id,
  external_id: `f${id}`,
  name,
  project,
  module: "pipeline",
  source_dir: `/home/me/${project}`,
  description: `${name} description`,
  tags: [],
  parameter_schema: { type: "object", properties: {} },
  options: {},
  live: true,
  last_seen_at: NOW,
  error: null,
  created_at: 0,
  recent_runs: [
    [10, "Running", "Running", null],
    [9, "Completed", "Completed", 4_000_000],
  ],
  triggers: [],
  triggered_by: null,
  upstreams: [],
  batch_key: null,
  schedules: [],
  ...extra,
});
const FLOWS = [
  flow(1, "daily_orders", "warehouse", { triggered_by: "stripe_sync", upstreams: ["stripe_sync"] }),
  flow(2, "customer_dim", "warehouse"),
  flow(3, "stripe_sync", "warehouse"),
  flow(4, "train_churn", "ml", {
    triggered_by: "a",
    upstreams: ["a", "b"],
    batch_key: "day",
  }),
  flow(5, "a", "ml"),
  flow(6, "b", "ml"),
  flow(7, "nightly_export", "legacy", { live: false, last_seen_at: NOW - 3 * 86_400_000_000 }),
];
const TASKS = [
  {
    id: 100,
    external_id: "t100",
    run_id: 10,
    name: "extract",
    task_key: "extract",
    dynamic_key: "extract-0",
    state: state("Completed"),
    failure_count: 0,
    crash_count: 0,
    created_at: NOW - 100_000_000,
    start_time: NOW - 100_000_000,
    end_time: NOW - 95_800_000,
    total_run_time: 4_200_000,
    run_name: "brisk-otter",
    flow_id: 1,
    flow_name: "daily_orders",
    project: "warehouse",
    parents: [],
  },
  {
    id: 101,
    external_id: "t101",
    run_id: 10,
    name: "validate",
    task_key: "validate",
    dynamic_key: "validate-0",
    state: state("Scheduled", "AwaitingRetry", {
      details: { attempt: 2, delay: 30, retries: 3 },
      timestamp: Date.now() * 1000,
    }),
    failure_count: 1,
    crash_count: 0,
    created_at: NOW - 90_000_000,
    start_time: NOW - 90_000_000,
    end_time: null,
    total_run_time: 6_300_000,
    run_name: "brisk-otter",
    flow_id: 1,
    flow_name: "daily_orders",
    project: "warehouse",
    parents: [],
  },
];

const calls: { method: string; url: string }[] = [];
function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}
function mockFetch() {
  calls.length = 0;
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(
      typeof input === "string" ? input : input instanceof URL ? input.href : input.url,
      "http://x",
    );
    const method = init?.method ?? (input instanceof Request ? input.method : "GET");
    calls.push({ method, url: url.pathname + url.search });
    const p = url.pathname;
    if (p === "/api/counts")
      return json({ runs: { Running: 1, Paused: 1, Scheduled: 1 }, task_runs: {}, active: 1, flows: {} });
    if (p === "/api/flows") return json(flowsOverride ?? FLOWS);
    if (p === "/api/settings")
      return json({ engine_saturation_risk: false, saturation_flows: [], served_dir: "~/pipelines" });
    if (p === "/api/runs") {
      const project = url.searchParams.get("project");
      const st = url.searchParams.get("state_type");
      return json({
        items: RUNS.filter((r) => (!project || r.project === project) && (!st || r.state.type === st)),
        next_cursor: null,
      });
    }
    if (/^\/api\/runs\/\d+\/tasks$/.test(p)) return json(TASKS);
    if (/^\/api\/runs\/\d+\/logs$/.test(p) || /^\/api\/task-runs\/\d+\/logs$/.test(p))
      return json({ items: [], next_cursor: null });
    if (/^\/api\/runs\/\d+\/cancel$/.test(p)) return json(RUNS[0]);
    if (/^\/api\/runs\/\d+$/.test(p)) {
      const id = Number(p.split("/").pop());
      const r = RUNS.find((x) => x.id === id);
      return r ? json(r) : json({ error: "run not found" }, 404);
    }
    if (p === "/api/artifacts" || /^\/api\/runs\/\d+\/artifacts$/.test(p))
      return json({ items: [], next_cursor: null });
    return json({ error: `unmocked ${method} ${p}` }, 404);
  });
}

let flowsOverride: ReturnType<typeof flow>[] | null = null;

function mount(path: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: [path] }),
    context: { queryClient: client },
  });
  const utils = render(
    <QueryClientProvider client={client}>
      <ProjectProvider>
        <RouterProvider router={router} />
      </ProjectProvider>
    </QueryClientProvider>,
  );
  return { ...utils, router };
}

beforeEach(() => {
  flowsOverride = null;
  localStorage.clear();
  document.documentElement.classList.remove("dark");
  mockFetch();
});
afterEach(() => {
  fetchMock.mockReset();
});

test("state badge colours are the 1.3 colours", () => {
  const { rerender } = render(<StateBadge state={state("Completed") as any} />);
  expect(screen.getByText("Completed").className).toContain("bg-emerald-100");
  rerender(<StateBadge state={state("Failed") as any} />);
  expect(screen.getByText("Failed").className).toContain("bg-red-100");
  rerender(<StateBadge state={state("Paused") as any} />);
  expect(screen.getByText("Paused").className).toContain("bg-violet-100");
  rerender(<StateBadge state={state("Completed", "Skipped") as any} />);
  expect(screen.getByText("Skipped").className).toContain("bg-teal-100");
});

test("the sidebar carries the sections in their groups, the scope picker only on list pages, and the theme toggle persists", async () => {
  const { router } = mount("/runs");
  const nav = await screen.findByRole("navigation", { name: "Sections" });
  expect(
    within(nav)
      .getAllByRole("link")
      .map((l) => l.textContent),
  ).toEqual([
    "Dashboard",
    "Runs",
    "Queue",
    "Flows",
    "Variables",
    "Rules",
    "Events",
    "Artifacts",
    "Workers",
    "Settings",
  ]);
  for (const label of ["Operate", "Build", "Observe", "System"]) {
    expect(within(nav).getByText(label)).toBeInTheDocument();
  }
  expect(within(nav).getByRole("link", { name: /^Runs/ })).toHaveAttribute("aria-current", "page");
  expect(screen.getByTestId("scope-picker")).toBeInTheDocument();
  // No section tabs in the top bar any more.
  expect(within(screen.getByTestId("top-bar-row")).queryByRole("link", { name: "Runs" })).toBeNull();
  // Workers is the Queue page's Workers tab, and is the entry marked there.
  fireEvent.click(within(nav).getByRole("link", { name: "Workers" }));
  await waitFor(() => expect(router.state.location.search).toEqual({ tab: "workers" }));
  expect(within(nav).getByRole("link", { name: "Workers" })).toHaveAttribute("aria-current", "page");
  expect(within(nav).getByRole("link", { name: /^Queue/ })).not.toHaveAttribute("aria-current");
  expect(screen.queryByTestId("scope-picker")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Toggle theme" }));
  await waitFor(() => expect(localStorage.getItem("cereyan-theme")).toBe("dark"));
  expect(document.documentElement.classList.contains("dark")).toBe(true);
});

test("project scope persists and a deep link overrides it", () => {
  function Probe({ override }: { override?: string }) {
    const { project, setProject } = useProject(override);
    return (
      <button type="button" onClick={() => setProject("warehouse")}>
        {project ?? "all"}
      </button>
    );
  }
  const { unmount } = render(
    <ProjectProvider>
      <Probe />
    </ProjectProvider>,
  );
  fireEvent.click(screen.getByRole("button"));
  expect(screen.getByRole("button")).toHaveTextContent("warehouse");
  expect(localStorage.getItem("cereyan-project")).toBe("warehouse");
  unmount();
  render(
    <ProjectProvider>
      <Probe override="ml" />
    </ProjectProvider>,
  );
  expect(screen.getByRole("button")).toHaveTextContent("ml");
});

test("command palette jumps to a section and to a run", async () => {
  const { router } = mount("/");
  await screen.findByRole("navigation", { name: "Sections" });
  fireEvent.keyDown(window, { key: "k", metaKey: true });
  const input = await screen.findByPlaceholderText("Search runs, flows, artifacts");
  fireEvent.click(within(screen.getByRole("dialog")).getByText("Rules"));
  await waitFor(() => expect(router.state.location.pathname).toBe("/rules"));
  fireEvent.keyDown(window, { key: "k", metaKey: true });
  fireEvent.change(await screen.findByPlaceholderText("Search runs, flows, artifacts"), {
    target: { value: "otter" },
  });
  fireEvent.click(await within(screen.getByRole("dialog")).findByText("brisk-otter", {}, { timeout: 2000 }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/runs/10"));
  expect(input).not.toBeInTheDocument();
});

test("filter popover closes on Escape and returns focus", async () => {
  const onChange = vi.fn();
  render(
    <FilterSelect
      label="State"
      value=""
      onChange={onChange}
      options={[{ value: "Failed", label: "Failed" }]}
    />,
  );
  const trigger = screen.getByRole("button", { name: "State" });
  fireEvent.click(trigger);
  await screen.findByText("Failed");
  fireEvent.keyDown(document.activeElement ?? trigger, { key: "Escape" });
  await waitFor(() => expect(screen.queryByText("Failed")).toBeNull());
  expect(document.activeElement).toBe(trigger);
});

test("dashboard counts Paused and Late, lists what needs attention, and Answer opens the run", async () => {
  const { router } = mount("/");
  await screen.findByTestId("task-progress");
  expect(screen.getByTestId("count-Paused")).toHaveTextContent("1");
  expect(screen.getByTestId("count-Late")).toHaveTextContent("1");
  expect(screen.getByTestId("count-Running")).toHaveTextContent("1");
  const rows = await screen.findAllByTestId("attention-row");
  expect(rows.map((r) => r.getAttribute("data-run-id"))).toEqual(["11", "12", "13"]);
  expect(screen.getByTestId("task-progress")).toHaveTextContent("7 of 12 tasks");
  fireEvent.click(screen.getByTestId("answer-run"));
  await waitFor(() => expect(router.state.location.pathname).toBe("/runs/11"));
  expect(await screen.findByTestId("paused-banner")).toHaveTextContent("Approve 1,204 deletes?");
});

test("run table draws task bars and the selection bar comes and goes", async () => {
  const { router } = mount("/runs?state=Running");
  await screen.findByText("brisk-otter");
  expect(screen.getByTestId("filter-state")).toHaveAttribute("data-value", "Running");
  // The group header carries a rollup bar of its own, so scope to the run's row.
  const row = screen.getByText("brisk-otter").closest("tr") as HTMLElement;
  const bar = within(row).getByTestId("state-bar");
  expect(Array.from(bar.children).map((c) => c.getAttribute("data-state"))).toEqual([
    "Completed",
    "Running",
    "Pending",
  ]);
  expect(screen.queryByTestId("selection-bar")).toBeNull();
  fireEvent.click(screen.getByRole("checkbox", { name: "Select brisk-otter" }));
  const toolbar = await screen.findByTestId("selection-bar");
  expect(toolbar).toHaveTextContent("1 selected");
  fireEvent.click(within(toolbar).getByRole("button", { name: "Cancel" }));
  await waitFor(() =>
    expect(calls.some((c) => c.method === "POST" && c.url === "/api/runs/10/cancel")).toBe(true),
  );
  await waitFor(() => expect(screen.queryByTestId("selection-bar")).toBeNull());
  expect(router.state.location.search).toMatchObject({ state: "Running" });
});

test("retry line and task rail selection", () => {
  const now = Date.now();
  const line = retryLine(
    { ...TASKS[1], state: { ...TASKS[1].state, timestamp: now * 1000 } } as any,
    now + 6000,
  );
  expect(line).toBe("retry 2 of 3 · in 24s");
  expect(retryLine(TASKS[0] as any, now)).toBeNull();
  const onSelect = vi.fn();
  render(<TaskRail tasks={TASKS as any} selectedId={undefined} onSelect={onSelect} />);
  expect(screen.getByTestId("retry-line")).toHaveTextContent(/retry 2 of 3/);
  expect(screen.getByText("1 of 2 completed")).toBeInTheDocument();
  expect(screen.getAllByTestId("task-state").map((e) => e.textContent)).toEqual([
    "Completed",
    "Awaiting retry",
  ]);
  fireEvent.click(screen.getByRole("option", { name: /validate-0/ }));
  expect(onSelect).toHaveBeenCalledWith(101);
});

test("run page: tasks rail filters logs, Delete lives in the overflow menu", async () => {
  mount("/runs/10");
  await screen.findByTestId("run-header");
  expect(screen.queryByRole("button", { name: "Delete" })).toBeNull();
  expect(screen.getByRole("button", { name: "More actions" })).toBeInTheDocument();
  expect(screen.getByTestId("run-facts")).toHaveTextContent("Triggered byschedule");
  fireEvent.click(await screen.findByRole("option", { name: /validate-0/ }));
  const chip = await screen.findByTestId("log-task-chip");
  expect(chip).toHaveTextContent("validate-0");
  await waitFor(() => expect(calls.some((c) => c.url.startsWith("/api/task-runs/101/logs"))).toBe(true));
  fireEvent.click(within(chip).getByRole("button", { name: "Show all tasks" }));
  await waitFor(() => expect(screen.queryByTestId("log-task-chip")).toBeNull());
});

test("flows page bands each group, shows the stale row, and still runs a flow", async () => {
  const { router } = mount("/flows");
  // Three projects and no declared groups, so each project is one band.
  const band = await screen.findByTestId("section-warehouse/warehouse");
  expect(within(band).getByTestId("section-meta")).toHaveTextContent(
    "3 flows · 0 scheduled · has dependencies",
  );
  expect(screen.getByRole("heading", { name: "All flows" })).toBeInTheDocument();
  const churn = screen.getByText("train_churn").closest("tr") as HTMLElement;
  expect(within(churn).getByTestId("starts-after")).toHaveTextContent("after a, b key=day");
  const stale = screen.getByText("nightly_export").closest("tr");
  expect(stale).toHaveAttribute("data-flow-live", "false");
  expect(stale).toHaveTextContent("Not registered by this server");
  expect(within(screen.getByTestId("section-legacy/legacy")).getByTestId("section-stale")).toHaveTextContent(
    "1 stale",
  );
  expect(within(stale as HTMLElement).getByRole("button", { name: "Delete" })).toBeInTheDocument();
  const row = screen.getByText("customer_dim").closest("tr") as HTMLElement;
  fireEvent.click(within(row).getByRole("button", { name: /Run/ }));
  const dialog = await screen.findByRole("dialog");
  expect(dialog).toHaveTextContent("Run warehouse/customer_dim");
  await act(async () => {
    fireEvent.click(within(dialog).getByRole("button", { name: /^Run$/ }));
  });
  await waitFor(() =>
    expect(calls.some((c) => c.method === "POST" && c.url === "/api/flows/2/runs")).toBe(true),
  );
  expect(router.state.location.pathname).toMatch(/\/(flows|runs)/);
});

test("flows page: a group name used in two projects is a scope under each", async () => {
  flowsOverride = [
    flow(1, "load", "warehouse", { group: "nightly" }),
    flow(2, "rollup", "analytics", { group: "nightly" }),
    flow(3, "adhoc", "warehouse"),
  ];
  mount("/flows");
  // One `nightly` band per project, each counting only that project's flows.
  const nightly = await screen.findByTestId("section-warehouse/nightly");
  expect(within(nightly).getByTestId("section-meta")).toHaveTextContent("1 flow ·");
  expect(screen.getByTestId("section-analytics/nightly")).toBeInTheDocument();
  // The picker lists the project's own flows beside its declared group.
  fireEvent.click(screen.getByTestId("scope-trigger"));
  expect(await screen.findByTestId("scope-group-warehouse/warehouse")).toHaveTextContent("Ungrouped flows1");
  fireEvent.click(screen.getByTestId("scope-group-warehouse/nightly"));
  // Scoped to one group: no bands, the group's summary, and the top bar follows.
  await waitFor(() => expect(screen.getByTestId("group-stats")).toBeInTheDocument());
  expect(screen.queryByTestId("section-warehouse/nightly")).toBeNull();
  expect(screen.getByText("load")).toBeInTheDocument();
  expect(screen.queryByText("rollup")).toBeNull();
  expect(screen.getByTestId("shell")).toHaveAttribute("data-scope-group", "nightly");
  expect(localStorage.getItem("cereyan-group")).toBe("nightly");
  expect(screen.getByTestId("flow-count")).toHaveTextContent("1 flows");
});

test("the scope picker narrows Runs by group, replacing a linked project, and stays off other pages", async () => {
  flowsOverride = [
    flow(1, "load", "warehouse", { group: "nightly" }),
    flow(2, "adhoc", "warehouse"),
    flow(3, "train", "ml"),
  ];
  const { router } = mount("/runs?project=ml");
  // The linked project is the one named, not the stored scope.
  const trigger = await screen.findByTestId("scope-trigger");
  expect(trigger).toHaveAccessibleName("Scope: ml");
  fireEvent.click(trigger);
  fireEvent.click(await screen.findByTestId("scope-group-warehouse/nightly"));
  await waitFor(() =>
    expect(
      calls.some(
        (c) =>
          c.url.startsWith("/api/runs?") && /project=warehouse/.test(c.url) && /group=nightly/.test(c.url),
      ),
    ).toBe(true),
  );
  expect(router.state.location.search).not.toHaveProperty("project");
  expect(screen.getByTestId("scope-trigger")).toHaveAccessibleName("Scope: warehouse › nightly");
  await act(async () => {
    await router.navigate({ to: "/rules" });
  });
  expect(screen.queryByTestId("scope-picker")).toBeNull();
  expect(screen.getByRole("navigation", { name: "Sections" })).toBeInTheDocument();
});

test("the scope picker filters projects and groups as the user types", async () => {
  flowsOverride = [
    flow(1, "load", "warehouse", { group: "nightly" }),
    flow(2, "adhoc", "warehouse", { group: "adhoc" }),
    flow(3, "train", "ml"),
  ];
  mount("/flows");
  fireEvent.click(await screen.findByTestId("scope-trigger"));
  fireEvent.change(await screen.findByPlaceholderText("Find a project or group"), {
    target: { value: "night" },
  });
  await waitFor(() => expect(screen.queryByTestId("scope-project-ml")).toBeNull());
  expect(screen.getByTestId("scope-group-warehouse/nightly")).toBeInTheDocument();
  expect(screen.queryByTestId("scope-group-warehouse/adhoc")).toBeNull();
});

test("New run picks a flow, then opens its run form", async () => {
  mount("/events");
  fireEvent.click(await screen.findByRole("button", { name: "New run" }));
  const dialog = await screen.findByRole("dialog");
  fireEvent.click(await within(dialog).findByText("customer_dim"));
  expect(await screen.findByText(/^Run .*\/customer_dim$/)).toBeInTheDocument();
});

test("flows page: facets narrow within the scope and count against it", async () => {
  mount("/flows");
  await screen.findByTestId("section-warehouse/warehouse");
  fireEvent.click(screen.getByRole("button", { name: "Tags" }));
  // No flow has tags, so the Tags facet opens empty and closes on Escape.
  fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
  fireEvent.change(screen.getByLabelText("Search flows"), { target: { value: "sync" } });
  expect(screen.getByTestId("flow-count")).toHaveTextContent("1 of 7 flows");
  expect(
    within(screen.getByTestId("section-warehouse/warehouse")).getByTestId("section-meta"),
  ).toHaveTextContent("1 of 3 flows");
  fireEvent.click(screen.getByRole("button", { name: "Clear" }));
  expect(screen.getByTestId("flow-count")).toHaveTextContent("7 flows");
});

test("flows page: a group band spans exactly the table's columns", async () => {
  // A cell too many in a row silently shifts every value out of its column.
  mount("/flows");
  const band = await screen.findByTestId("section-warehouse/warehouse");
  const width = (tr: HTMLElement) =>
    Array.from(tr.querySelectorAll(":scope > td")).reduce(
      (n, td) => n + Number(td.getAttribute("colspan") ?? 1),
      0,
    );
  const dataRow = screen.getByText("customer_dim").closest("tr") as HTMLElement;
  const headings = document.querySelectorAll("thead th").length;
  expect(width(band)).toBe(headings);
  expect(width(dataRow)).toBe(headings);
});
