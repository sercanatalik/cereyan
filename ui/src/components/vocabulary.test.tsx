// The rule form offers the catalogue without closing the field, and the rules
// list marks a rule that has never matched anything.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { routeTree } from "@/routeTree.gen";
import { RuleForm } from "./rule-form";

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

const VOCABULARY = {
  events: [
    { name: "run.completed", resource: "run", when: "", payload_fields: [] },
    { name: "run.failed", resource: "run", when: "", payload_fields: [] },
    { name: "task_run.failed", resource: "task_run", when: "", payload_fields: [] },
  ],
  reserved_prefixes: ["run.", "task_run."],
  states: [
    { name: "Failed", state_type: "Failed", is_sub_state: false },
    { name: "Scheduled", state_type: "Scheduled", is_sub_state: false },
    { name: "TimedOut", state_type: "Failed", is_sub_state: true },
  ],
};

const RULES = [
  {
    id: 1,
    name: "alerts",
    enabled: true,
    source: "ui",
    module: null,
    fire_count: 12,
    last_fired: 1_700_000_000_000_000,
    created_at: 0,
    updated_at: 0,
    when: { events: ["run.failed"], flows: [], tags: [], states: [], project: null },
    do: [{ kind: "cancel_run" }],
    once: "per_run",
    cooldown_seconds: 0,
    max_per_minute: 60,
    allow_self: false,
    unless: null,
    within: null,
    at: null,
  },
  {
    id: 2,
    name: "quiet",
    enabled: true,
    source: "ui",
    module: null,
    fire_count: 0,
    last_fired: null,
    created_at: 0,
    updated_at: 0,
    when: { events: ["orders.nothing"], flows: [], tags: [], states: [], project: null },
    do: [{ kind: "cancel_run" }],
    once: "per_run",
    cooldown_seconds: 0,
    max_per_minute: 60,
    allow_self: false,
    unless: null,
    within: null,
    at: null,
  },
];

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

beforeEach(() => {
  fetchMock.mockReset();
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(
      typeof input === "string" ? input : input instanceof URL ? input.href : input.url,
      "http://x",
    );
    const p = url.pathname;
    if (p === "/api/vocabulary") return json(VOCABULARY);
    if (p === "/api/rules") return json(RULES);
    if (p === "/api/counts") return json({ runs: {}, task_runs: {}, active: 0, flows: {} });
    if (p === "/api/flows") return json([]);
    if (p === "/api/settings") return json({ engine_saturation_risk: false, saturation_flows: [] });
    return json({ error: `unmocked ${init?.method ?? "GET"} ${p}` }, 404);
  });
});

function renderForm(props: Partial<React.ComponentProps<typeof RuleForm>> = {}) {
  return render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <RuleForm onSave={() => {}} {...props} />
    </QueryClientProvider>,
  );
}

test("the events field suggests the catalogue", async () => {
  renderForm();
  const field = screen.getByLabelText(/Events/);
  fireEvent.focus(field);
  fireEvent.change(field, { target: { value: "run." } });
  const box = await screen.findByTestId("rule-events-suggestions");
  expect(box).toHaveTextContent("run.failed");
  expect(box).toHaveTextContent("run.completed");
  expect(box).toHaveTextContent("run.*");
  // task_run.failed contains "run." too, which is the point of a substring match.
  expect(box).toHaveTextContent("task_run.failed");
});

test("clicking a suggestion appends it and leaves earlier names alone", async () => {
  const saved: unknown[] = [];
  renderForm({ onSave: (r) => saved.push(r) });
  const field = screen.getByLabelText(/Events/) as HTMLInputElement;
  fireEvent.focus(field);
  fireEvent.change(field, { target: { value: "orders.low, run.fail" } });
  const box = await screen.findByTestId("rule-events-suggestions");
  fireEvent.click(within(box).getByText("run.failed"));
  await waitFor(() => expect(field.value).toBe("orders.low, run.failed, "));
});

test("a custom name is still accepted", async () => {
  const saved: any[] = [];
  renderForm({ onSave: (r) => saved.push(r) });
  const field = screen.getByLabelText(/Events/);
  fireEvent.change(field, { target: { value: "orders.table_empty" } });
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "custom" } });
  fireEvent.click(screen.getByRole("button", { name: /save/i }));
  await waitFor(() => expect(saved).toHaveLength(1));
  expect(saved[0].when.events).toEqual(["orders.table_empty"]);
});

test("the states field suggests types and sub-states", async () => {
  renderForm();
  const field = screen.getByLabelText(/States/);
  fireEvent.focus(field);
  fireEvent.change(field, { target: { value: "" } });
  const box = await screen.findByTestId("rule-states-suggestions");
  expect(box).toHaveTextContent("Failed");
  expect(box).toHaveTextContent("TimedOut");
});

test("a rejected name is shown on the form", () => {
  renderForm({ error: 'when: unknown event name "run.failure"; did you mean "run.failed"?' });
  expect(screen.getByText(/did you mean "run.failed"/)).toBeInTheDocument();
});

test("the rules list marks a rule that has never fired", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: ["/rules"] }),
    context: { queryClient: client },
  });
  render(
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  await screen.findByText("alerts");
  const markers = screen.getAllByTestId("rule-never-fired");
  expect(markers).toHaveLength(1);
  expect(markers[0]).toHaveTextContent("never fired");
});
