// Skipping upcoming fires and rescheduling: the Flows menu, the Skip and
// Reschedule dialogs, and the flow page's Upcoming tab.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ProjectProvider } from "@/lib/project";
import { type CronShape, formatCronShape, parseCronShape } from "@/lib/schedule-shape";
import { formatFire } from "@/lib/utils";
import { routeTree } from "@/routeTree.gen";
import { RescheduleDialog } from "./reschedule-dialog";
import { SkipDialog } from "./skip-dialog";

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
const HOUR = 3_600_000_000;
const DAY = 24 * HOUR;
const FIRST = NOW + HOUR;

const schedule = (extra: Record<string, unknown> = {}) => ({
  id: 5,
  external_id: "s5",
  flow_id: 1,
  schedule: { kind: "cron", cron: "30 6 * * 1-5", timezone: "UTC", day_or: true },
  catchup: "skip",
  catchup_max: 100,
  active: true,
  paused_reason: null,
  paused_until: null,
  source: "code",
  code_key: "code-0",
  persist: false,
  created_at: 0,
  updated_at: 0,
  next_fire: FIRST,
  skipped: 1,
  ...extra,
});
const flow = (id: number, name: string, extra: Record<string, unknown> = {}) => ({
  id,
  external_id: `f${id}`,
  name,
  project: "warehouse",
  module: "pipeline",
  source_dir: "/home/me/warehouse",
  description: null,
  tags: [],
  parameter_schema: { type: "object", properties: {} },
  options: {},
  live: true,
  last_seen_at: NOW,
  error: null,
  created_at: 0,
  recent_runs: [],
  triggers: [],
  triggered_by: null,
  upstreams: [],
  batch_key: null,
  schedules: [],
  ...extra,
});
const FLOWS = [
  flow(1, "sales", { schedules: [schedule()], triggers: ["report"] }),
  flow(2, "report", { triggered_by: "sales", upstreams: ["sales"] }),
  flow(3, "adhoc"),
];
const runItem = (id: number, time: number, skipped = false) => ({
  id,
  external_id: `r${id}`,
  flow_id: 1,
  flow_name: "sales",
  project: "warehouse",
  name: `sales-${id}`,
  parameters: {},
  tags: [],
  state: {
    type: "Scheduled",
    name: "Scheduled",
    message: null,
    details: skipped ? { skip: "user" } : {},
    timestamp: NOW,
  },
  failure_count: 0,
  crash_count: 0,
  created_at: NOW,
  start_time: null,
  end_time: null,
  total_run_time: null,
  engine_pid: null,
  engine_id: null,
  created_by: "schedule",
  report_seq: 0,
  attempt: 1,
  priority: 0,
  task_counts: {},
  schedule_id: 5,
  scheduled_time: time,
  skipped,
  skipped_by: skipped ? "ui" : null,
  skipped_at: skipped ? NOW - HOUR : null,
  projected: false,
});
const projected = (time: number) => ({
  schedule_id: 5,
  scheduled_time: time,
  skipped: false,
  projected: true,
});
const UPCOMING = [
  runItem(21, FIRST),
  runItem(22, FIRST + DAY, true),
  runItem(23, FIRST + 2 * DAY),
  runItem(24, FIRST + 3 * DAY),
  projected(FIRST + 4 * DAY),
  projected(FIRST + 5 * DAY),
];

const calls: { method: string; url: string; body?: unknown }[] = [];
function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}
function mockFetch() {
  calls.length = 0;
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(
      typeof input === "string" ? input : input instanceof URL ? input.href : input.url,
      "http://x",
    );
    const method = init?.method ?? request?.method ?? "GET";
    const text = request ? await request.clone().text() : typeof init?.body === "string" ? init.body : "";
    const body = text ? JSON.parse(text) : undefined;
    calls.push({ method, url: url.pathname + url.search, body });
    const p = url.pathname;
    if (p === "/api/flows") return json(FLOWS);
    if (p === "/api/flows/1") return json(FLOWS[0]);
    if (p === "/api/flows/1/upcoming") return json(UPCOMING);
    if (p === "/api/runs") return json({ items: [], next_cursor: null });
    if (p === "/api/counts") return json({ runs: {}, task_runs: {}, active: 0, flows: {} });
    if (p === "/api/settings")
      return json({ engine_saturation_risk: false, saturation_flows: [], served_dir: null });
    if (p === "/api/schedules/preview")
      return json({ next: [0, 1, 2, 3, 4].map((i) => FIRST + i * DAY), timezone: "UTC" });
    if (p === "/api/schedules/5/skips")
      return json({ schedule: schedule({ skipped: 2 }), skipped: body?.fires ?? [], downstream: [] });
    if (/^\/api\/schedules\/5\/skips\/\d+$/.test(p)) return json(schedule({ skipped: 0 }));
    if (p === "/api/schedules/5") return json(schedule());
    return json({ error: `unmocked ${method} ${p}` }, 404);
  });
}

function withClient(ui: React.ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

function mount(path: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: [path] }),
    context: { queryClient: client },
  });
  return render(
    <QueryClientProvider client={client}>
      <ProjectProvider>
        <RouterProvider router={router} />
      </ProjectProvider>
    </QueryClientProvider>,
  );
}

const posted = (url: string) => calls.find((c) => c.method === "POST" && c.url === url)?.body;

beforeEach(() => {
  localStorage.clear();
  mockFetch();
});
afterEach(() => {
  fetchMock.mockReset();
});

test("the reschedule fields round-trip cron shapes", () => {
  expect(parseCronShape("30 6 * * 1-5")).toEqual({ minute: 30, hour: 6, days: [1, 2, 3, 4, 5] });
  expect(formatCronShape({ minute: 30, hour: 6, days: [1, 2, 3, 4, 5, 6] })).toBe("30 6 * * 1-6");
  expect(formatCronShape(parseCronShape("0 9 * * *") as CronShape)).toBe("0 9 * * *");
  expect(formatCronShape({ minute: 0, hour: 9, days: [0, 6] })).toBe("0 9 * * 0,6");
  expect(formatCronShape({ minute: 5, hour: 7, days: [1, 3, 4, 5] })).toBe("5 7 * * 1,3-5");
  expect(parseCronShape("0 9 * * 7")?.days).toEqual([0]);
  expect(parseCronShape("*/5 * * * *")).toBeNull();
  expect(parseCronShape("0 9 1 * *")).toEqual({
    minute: 0,
    hour: 9,
    days: [0, 1, 2, 3, 4, 5, 6],
    monthDay: 1,
  });
  expect(formatCronShape({ minute: 0, hour: 9, days: [], monthDay: 15 })).toBe("0 9 15 * *");
  expect(parseCronShape("0 9 1 * 1")).toBeNull();
  expect(parseCronShape("0 9 32 * *")).toBeNull();
});

test("the flows menu offers skip and reschedule only for scheduled flows", async () => {
  mount("/flows");
  expect(await screen.findByText(/1 skipped/)).toBeInTheDocument();
  fireEvent.keyDown(await screen.findByRole("button", { name: "More actions for sales" }), { key: "Enter" });
  const skipNext = await screen.findByRole("menuitem", { name: /Skip next run/ });
  expect(skipNext).toHaveTextContent(formatFire(FIRST));
  expect(screen.getByRole("menuitem", { name: "Skip runs…" })).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Reschedule…" })).toBeInTheDocument();
  fireEvent.click(skipNext);
  await waitFor(() => expect(posted("/api/schedules/5/skips")).toEqual({ next: 1, by: "ui" }));

  fireEvent.keyDown(screen.getByRole("button", { name: "More actions for adhoc" }), { key: "Enter" });
  await screen.findByRole("menuitem", { name: "Open flow" });
  expect(screen.queryByRole("menuitem", { name: /Skip/ })).toBeNull();
  expect(screen.queryByRole("menuitem", { name: /Reschedule/ })).toBeNull();
});

test("the skip dialog stepper ticks the next open fires and names the downstream", async () => {
  withClient(<SkipDialog flow={FLOWS[0] as any} open onClose={() => {}} />);
  await waitFor(() => expect(screen.getAllByTestId("skip-fire")).toHaveLength(6));
  fireEvent.click(screen.getByRole("button", { name: "More" }));
  expect(screen.getByTestId("skip-count")).toHaveTextContent("2");
  // The second fire is already skipped, so the stepper passes over it.
  expect(screen.getByTestId("skip-next-run")).toHaveTextContent(formatFire(FIRST + 3 * DAY));
  expect(await screen.findByTestId("skip-downstream")).toHaveTextContent("2 runs of report");
  fireEvent.click(screen.getByRole("button", { name: "Skip 2 runs" }));
  await waitFor(() =>
    expect(posted("/api/schedules/5/skips")).toEqual({ fires: [FIRST, FIRST + 2 * DAY], by: "ui" }),
  );
});

test("the reschedule dialog edits a cron shape and says a code edit ends at restart", async () => {
  withClient(<RescheduleDialog flow={FLOWS[0] as any} open onClose={() => {}} />);
  expect(await screen.findByTestId("reschedule-code-note")).toHaveTextContent("until the server restarts");
  expect(screen.getByLabelText("Time")).toHaveValue("06:30");
  expect(screen.getByRole("button", { name: "Mon" })).toHaveAttribute("aria-pressed", "true");
  expect(screen.getByRole("button", { name: "Sat" })).toHaveAttribute("aria-pressed", "false");
  fireEvent.click(screen.getByRole("button", { name: "Sat" }));
  expect(screen.getByTestId("reschedule-cron-text")).toHaveTextContent("30 6 * * 1-6");
  fireEvent.click(screen.getByRole("tab", { name: "Monthly" }));
  expect(screen.getByTestId("reschedule-cron-text")).toHaveTextContent("30 6 1 * *");
  fireEvent.click(screen.getByRole("tab", { name: "Weekly" }));
  expect(screen.getByTestId("reschedule-cron-text")).toHaveTextContent("30 6 * * 1-6");
  // Three runs of this schedule are waiting and one fire is skipped.
  expect(await screen.findByTestId("reschedule-impact")).toHaveTextContent("3 scheduled runs are replaced");
  fireEvent.click(screen.getByRole("button", { name: "Edit as cron" }));
  expect(screen.getByLabelText("Cron expression")).toHaveValue("30 6 * * 1-6");
  fireEvent.click(screen.getByRole("button", { name: "Save schedule" }));
  await waitFor(() =>
    expect(calls.find((c) => c.method === "PATCH")?.body).toEqual({
      cron: "30 6 * * 1-6",
      day_or: true,
      timezone: "UTC",
    }),
  );
});

test("the upcoming tab lists projected fires, undoes a skip, and skips two at once", async () => {
  mount("/flows/1");
  fireEvent.click(await screen.findByRole("tab", { name: /Upcoming/ }));
  expect(await screen.findByTestId("look-ahead-divider")).toBeInTheDocument();
  expect(screen.getAllByText("not created yet")).toHaveLength(2);
  expect(screen.getByText(/Skipped in the UI/)).toBeInTheDocument();
  // Select all takes every run that can still be skipped: three runs and two projected fires.
  fireEvent.click(screen.getByRole("checkbox", { name: "Select all upcoming runs" }));
  expect(screen.getByRole("button", { name: "Skip 5 runs" })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Clear" }));
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  await waitFor(() =>
    expect(
      calls.some((c) => c.method === "DELETE" && c.url === `/api/schedules/5/skips/${FIRST + DAY}`),
    ).toBe(true),
  );
  fireEvent.click(screen.getByRole("checkbox", { name: `Select ${formatFire(FIRST + 4 * DAY)}` }));
  fireEvent.click(screen.getByRole("checkbox", { name: `Select ${formatFire(FIRST)}` }));
  fireEvent.click(screen.getByRole("button", { name: "Skip 2 runs" }));
  await waitFor(() =>
    expect(posted("/api/schedules/5/skips")).toEqual({ fires: [FIRST, FIRST + 4 * DAY], by: "ui" }),
  );
});
