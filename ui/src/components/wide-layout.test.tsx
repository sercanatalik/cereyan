// The shared frame, the page widths, and the Events and Dashboard layouts at laptop and wide widths.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ProjectProvider } from "@/lib/project";
import { eventDotClass } from "@/routes/events";
import { routeTree } from "@/routeTree.gen";
import { Page } from "./shell";

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

const NOW = Date.now() * 1000;
const MINUTE = 60_000_000;
const midnight = new Date();
midnight.setHours(0, 0, 0, 0);
const YESTERDAY = (midnight.getTime() - 3_600_000) * 1000;

const event = (id: number, name: string, occurred: number) => ({
  id,
  seq: id,
  name,
  occurred,
  flow_id: 1,
  resource: { kind: name.split(".")[0], id: `r${id}`, name: `res-${id}` },
  related: [{ kind: "flow", id: "proj/etl", name: "etl" }],
  payload: { n: id },
});
// Today's events come first, a little before now, then one from yesterday.
const EVENTS = [
  event(3, "run.failed", Math.max(NOW - MINUTE, midnight.getTime() * 1000 + 2)),
  event(2, "rule.fired", Math.max(NOW - 2 * MINUTE, midnight.getTime() * 1000 + 1)),
  event(1, "run.completed", YESTERDAY),
];

const run = (id: number, name: string, type: string, endedAgo: number) => ({
  id,
  external_id: `x${id}`,
  flow_id: 1,
  flow_name: "etl",
  project: "proj",
  name,
  parameters: { day: "2026-09-14" },
  tags: [],
  state: { type, name: type, message: null, details: {}, timestamp: NOW - endedAgo },
  created_at: NOW - 60 * MINUTE,
  start_time: NOW - 60 * MINUTE,
  end_time: type === "Running" ? null : NOW - endedAgo,
  total_run_time: type === "Running" ? null : 1_000_000,
  task_counts: { Completed: 1 },
  created_by: "schedule",
  failure_count: 0,
  crash_count: 0,
});
// Ten completed runs, `done-1` the latest to finish, plus a failed and a running one.
const RUNS = [
  ...Array.from({ length: 10 }, (_, i) => run(i + 1, `done-${i + 1}`, "Completed", (i + 1) * MINUTE)),
  run(20, "broke", "Failed", 3 * MINUTE),
  run(21, "busy", "Running", 0),
];

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  setViewport(1280);
  fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
    const url = new URL(input instanceof Request ? input.url : String(input), "http://x");
    const p = url.pathname;
    if (p === "/api/flows")
      return json([{ id: 1, name: "etl", project: "proj", live: true, recent_runs: [] }]);
    if (p === "/api/events") return json({ items: EVENTS, next_cursor: null });
    if (p === "/api/counts") return json({ runs: { Running: 1 }, task_runs: {}, active: 1, flows: {} });
    if (p === "/api/runs")
      return json({
        items: url.searchParams.get("state_type") === "Scheduled" ? [] : RUNS,
        next_cursor: null,
      });
    return json({ error: `unmocked ${p}` }, 404);
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
  return router;
}

test("a fluid page sits on the frame instead of a centred 1280px column", () => {
  const { container } = render(<Page title="Runs">rows</Page>);
  const page = container.firstElementChild as HTMLElement;
  expect(page).toHaveAttribute("data-width", "fluid");
  expect(page.className).toContain("frame");
  expect(page.className).not.toContain("max-w-[1280px]");
});

test("a narrow page caps its content at 1080px on the frame's left edge", () => {
  const { container } = render(
    <Page title="Settings" width="narrow">
      cards
    </Page>,
  );
  const page = container.firstElementChild as HTMLElement;
  const inner = page.firstElementChild as HTMLElement;
  expect(page.className).toContain("frame");
  expect(inner.className).toContain("max-w-[1080px]");
  expect(inner.className).not.toContain("mx-auto");
});

test("a bleed page runs edge to edge", () => {
  const { container } = render(<Page width="bleed">band</Page>);
  const page = container.firstElementChild as HTMLElement;
  expect(page).toHaveAttribute("data-width", "bleed");
  expect(page.className).not.toContain("frame");
});

test("the top bar's row sits on the same frame as the page", async () => {
  mount("/events");
  expect((await screen.findByTestId("top-bar-row")).className).toContain("frame");
});

test("day rows separate today from yesterday and event rows show only the time of day", async () => {
  mount("/events");
  const days = await screen.findAllByTestId("day-row");
  expect(days.map((d) => d.textContent)).toEqual([
    expect.stringContaining("Today"),
    expect.stringContaining("Yesterday"),
  ]);
  const row = document.querySelector('[data-event-name="run.failed"]') as HTMLElement;
  expect(row.textContent).not.toContain(String(new Date().getFullYear()));
  expect(row.textContent).toContain("proj/etl");
});

test("a state in the event name colours its dot, and other names get a neutral one", async () => {
  expect(eventDotClass("run.failed")).toBe("bg-red-500");
  expect(eventDotClass("task_run.completed")).toBe("bg-emerald-500");
  expect(eventDotClass("rule.fired")).toContain("muted-foreground");
  mount("/events");
  await screen.findAllByTestId("day-row");
  const dot = (name: string) =>
    within(document.querySelector(`[data-event-name="${name}"]`) as HTMLElement).getByTestId("event-dot");
  expect(dot("run.failed").className).toContain("bg-red-500");
  expect(dot("rule.fired").className).toContain("muted-foreground");
});

test("from 1440px the detail panel stays open and still creates a rule from the event", async () => {
  setViewport(1440);
  const router = mount("/events");
  expect(await screen.findByText("Select an event")).toBeInTheDocument();
  // The panel shows before the events load; wait for the rows before choosing one.
  await screen.findAllByTestId("day-row");
  fireEvent.click(document.querySelector('[data-event-name="run.failed"]') as HTMLElement);
  fireEvent.click(await screen.findByRole("button", { name: "Create rule from this event" }));
  await waitFor(() =>
    expect(router.state.location.search).toMatchObject({ new: "1", events: "run.failed", flow: "etl" }),
  );
});

test("below 1440px the panel opens only when an event is selected", async () => {
  mount("/events");
  await screen.findAllByTestId("day-row");
  expect(screen.queryByTestId("event-drawer")).toBeNull();
  fireEvent.click(document.querySelector('[data-event-name="run.failed"]') as HTMLElement);
  expect(await screen.findByTestId("event-drawer")).toBeInTheDocument();
});

test("Recently completed lists the eight latest Completed runs, newest first", async () => {
  mount("/");
  const card = await screen.findByTestId("recently-completed");
  await waitFor(() => expect(within(card).getAllByRole("row")).toHaveLength(9));
  const names = within(card)
    .getAllByRole("link")
    .map((a) => a.textContent);
  expect(names).toEqual(["done-1", "done-2", "done-3", "done-4", "done-5", "done-6", "done-7", "done-8"]);
  expect(within(card).queryByText("broke")).toBeNull();
});

test("at 1440px the lists keep two columns and the histogram 24 buckets", async () => {
  setViewport(1440);
  mount("/");
  const lists = await screen.findByTestId("dashboard-lists");
  expect(lists.className).toContain("grid-cols-[minmax(0,7fr)_minmax(0,5fr)]");
  expect(screen.getByRole("img", { name: "Run activity" })).toHaveAttribute("data-buckets", "24");
  expect(screen.getByRole("columnheader", { name: "When" })).toBeInTheDocument();
});

test("from 1680px the three lists share a row and the histogram doubles its buckets", async () => {
  setViewport(2560);
  mount("/");
  const lists = await screen.findByTestId("dashboard-lists");
  expect(lists.className).toContain("grid-cols-3");
  expect(within(lists).getByText("Upcoming")).toBeInTheDocument();
  expect(screen.getByRole("img", { name: "Run activity" })).toHaveAttribute("data-buckets", "48");
});
