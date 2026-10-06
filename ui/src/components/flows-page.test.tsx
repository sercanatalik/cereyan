// The Flows page: quick-filter chips counted against the scope, the row's
// sub-line with health in words, the run strip and legend, and the subtitle.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
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
const schedule = {
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
  next_fire: NOW + 3_600_000_000,
  skipped: 0,
};
const done = (id: number) => [id, "Completed", "Completed", 2_000_000];
const flow = (id: number, name: string, project: string, extra: Record<string, unknown> = {}) => ({
  id,
  external_id: `f${id}`,
  name,
  project,
  module: "pipeline",
  source_dir: `/home/me/${project}`,
  description: null,
  tags: [],
  parameter_schema: { type: "object", properties: {} },
  options: {},
  live: true,
  last_seen_at: NOW,
  error: null,
  created_at: 0,
  recent_runs: [done(id * 10)],
  triggers: [],
  triggered_by: null,
  upstreams: [],
  batch_key: null,
  schedules: [],
  ...extra,
});

let FLOWS: ReturnType<typeof flow>[] = [];

/** Five warehouse flows, two scheduled and one tagged, and fourteen elsewhere. */
function nineteen() {
  return [
    flow(1, "daily_sales", "warehouse", { schedules: [schedule], tags: ["finance"] }),
    flow(2, "build_report", "warehouse", {
      schedules: [{ ...schedule, id: 6 }],
      upstreams: ["daily_sales"],
      triggered_by: "daily_sales",
      description: "The morning report\nwith more lines",
      recent_runs: [[21, "Failed", "Failed", 41_000], done(20), done(19)],
      health: { status: "FAIL", reasons: ["target is 3 hours old"] },
    }),
    flow(3, "customer_dim", "warehouse", {
      recent_runs: [[31, "Paused", "Paused", null]],
      health: { status: "PASS", reasons: [] },
    }),
    flow(4, "orders_dim", "warehouse", { health: { status: "WARN", reasons: ["deadline in 10 minutes"] } }),
    flow(5, "adhoc", "warehouse", { recent_runs: [] }),
    ...Array.from({ length: 14 }, (_, i) => flow(100 + i, `ml_${i}`, "ml")),
  ];
}

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  localStorage.clear();
  FLOWS = nineteen();
  fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
    const request = input instanceof Request ? input : null;
    const p = new URL(request ? request.url : String(input), "http://x").pathname;
    if (p === "/api/flows") return json(FLOWS);
    if (p === "/api/settings") return json({ engine_saturation_risk: false, saturation_flows: [] });
    if (p === "/api/server")
      return json({ title: "cereyan", queued: 0, engines: [], paused: null, exposed: false, auth: false });
    if (p === "/api/counts") return json({ runs: {}, task_runs: {}, active: 0, flows: {} });
    return json({ error: `unmocked ${p}` }, 404);
  });
});

function mount(path = "/flows") {
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

/** The flows listed, by name, in table order. */
const listed = () => screen.getAllByTestId("flow-name").map((a) => a.textContent);
/** A flow's row, found by its name link. */
const rowOf = (name: string) =>
  (screen.getAllByTestId("flow-name").find((a) => a.textContent === name) as HTMLElement).closest(
    "tr",
  ) as HTMLElement;
const chip = (value: string) => screen.getByTestId(`chip-${value}`);
const chipCount = (value: string) => within(chip(value)).getByTestId("chip-count").textContent;

test("the chips are All, Failing, Scheduled, Waiting for input and Never run, with counts", async () => {
  mount();
  await screen.findByText("customer_dim");
  const group = screen.getByRole("group", { name: "Quick filters" });
  expect(
    within(group)
      .getAllByRole("button")
      .map((b) => b.textContent),
  ).toEqual(["All19", "Failing1", "Scheduled2", "Waiting for input1", "Never run1"]);
  expect(chip("all")).toHaveAttribute("aria-pressed", "true");
  // The Last run and Schedule facets are gone; Tags stays.
  expect(screen.queryByRole("button", { name: /^Last run/ })).toBeNull();
  expect(screen.queryByRole("button", { name: /^Schedule(?!d)/ })).toBeNull();
  expect(screen.getByRole("button", { name: /^Tags/ })).toBeInTheDocument();
});

test("chips count against the scope, not the narrowed list", async () => {
  localStorage.setItem("cereyan-project", "warehouse");
  mount();
  await screen.findByText("customer_dim");
  expect(screen.getByTestId("flow-count")).toHaveTextContent("5 flows");
  fireEvent.click(screen.getByRole("button", { name: /^Tags/ }));
  fireEvent.click(await screen.findByRole("option", { name: /finance/ }));
  await waitFor(() => expect(screen.getByTestId("flow-count")).toHaveTextContent("1 of 5 flows"));
  expect(chipCount("scheduled")).toBe("2");
  expect(chipCount("all")).toBe("5");
});

test("the Failing chip lists only the failing flow under its band", async () => {
  mount();
  await screen.findByText("customer_dim");
  fireEvent.click(chip("failing"));
  expect(chip("failing")).toHaveAttribute("aria-pressed", "true");
  expect(screen.getByTestId("flow-count")).toHaveTextContent("1 of 19 flows");
  const band = screen.getByTestId("section-warehouse/warehouse");
  expect(within(band).getByTestId("section-meta")).toHaveTextContent("1 of 5 flows");
  expect(listed()).toEqual(["build_report"]);
  expect(screen.queryByTestId("section-ml/ml")).toBeNull();
  // Clear returns to All.
  fireEvent.click(screen.getByRole("button", { name: "Clear" }));
  expect(chip("all")).toHaveAttribute("aria-pressed", "true");
  expect(screen.getByTestId("flow-count")).toHaveTextContent("19 flows");
});

test("Waiting for input, Scheduled and Never run each narrow the list", async () => {
  mount();
  await screen.findByText("customer_dim");
  fireEvent.click(chip("waiting"));
  expect(screen.getByTestId("flow-count")).toHaveTextContent("1 of 19 flows");
  expect(listed()).toEqual(["customer_dim"]);
  fireEvent.click(chip("scheduled"));
  expect(screen.getByTestId("flow-count")).toHaveTextContent("2 of 19 flows");
  fireEvent.click(chip("never"));
  expect(screen.getByTestId("flow-count")).toHaveTextContent("1 of 19 flows");
  expect(listed()).toEqual(["adhoc"]);
});

test("filters reset when the scope changes", async () => {
  mount();
  await screen.findByText("customer_dim");
  fireEvent.click(chip("never"));
  expect(screen.getByTestId("flow-count")).toHaveTextContent("1 of 19 flows");
  fireEvent.click(within(screen.getByTestId("section-warehouse/warehouse")).getByRole("button"));
  await waitFor(() => expect(chip("all")).toHaveAttribute("aria-pressed", "true"));
  expect(screen.getByTestId("flow-count")).toHaveTextContent("5 flows");
});

test("the sub-line holds description, upstream, tags and health in words", async () => {
  mount();
  await screen.findByText("build_report");
  const row = rowOf("build_report");
  expect(row).toHaveTextContent("The morning report");
  expect(row).not.toHaveTextContent("with more lines");
  expect(within(row).getByTestId("starts-after")).toHaveTextContent("after daily_sales");
  const health = within(row).getByTestId("flow-health");
  expect(health).toHaveTextContent("Health check failing: target is 3 hours old");
  expect(health).toHaveAttribute("data-failing", "true");
  expect(health.className).toMatch(/text-red-/);
  const tagged = rowOf("daily_sales");
  expect(within(tagged).getByTestId("flow-tags")).toHaveTextContent("finance");
  expect(within(rowOf("customer_dim")).getByTestId("flow-health")).toHaveTextContent("Health check passing");
  const warn = within(rowOf("orders_dim")).getByTestId("flow-health");
  expect(warn).toHaveTextContent("Health check warning: deadline in 10 minutes");
  expect(warn.className).not.toMatch(/text-red-/);
  // A flow without freshness or deadline options shows no health.
  expect(within(rowOf("adhoc")).queryByTestId("flow-health")).toBeNull();
  // The old columns are gone.
  const headings = Array.from(document.querySelectorAll("thead th")).map((th) => th.textContent);
  expect(headings).toEqual(["Flow", "Schedule", "Last 10 runs", "Last run", ""]);
});

test("each row draws a ten-slot run strip, and a legend names its colours", async () => {
  mount();
  await screen.findByText("build_report");
  const row = rowOf("build_report");
  const strip = within(row).getByTestId("run-strip");
  const slots = Array.from(strip.children) as HTMLElement[];
  expect(slots.map((s) => s.dataset.state)).toEqual([
    ...Array(7).fill("none"),
    "Completed",
    "Completed",
    "Failed",
  ]);
  expect(slots[9]).toHaveAttribute("href", "/runs/21");
  expect(screen.queryByTestId("run-sparkline")).toBeNull();
  expect(screen.getByText("Waiting")).toBeInTheDocument();
});

test("the subtitle counts failing and waiting flows in scope", async () => {
  mount();
  expect(await screen.findByText(/19 flows in 2 projects/)).toHaveTextContent(
    "19 flows in 2 projects · 1 is failing, 1 is waiting for input",
  );
});

test("a quiet scope's subtitle says nothing about failing", async () => {
  localStorage.setItem("cereyan-project", "ml");
  mount();
  const subtitle = await screen.findByText(/14 flows in 1 group/);
  expect(subtitle).not.toHaveTextContent("failing");
});
