import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { countRuns, dateParameters, intervalSeconds } from "./backfill-dialog";
import { StateBadge } from "./ported/state-badge";
import { coerceValue, defaultsFromSchema, RunForm } from "./run-form";
import { cronDescription, draftToBody, ScheduleEditor } from "./schedule-editor";
import { dependencyLevels, Timeline } from "./timeline";

function withRouter(element: React.ReactNode) {
  const root = createRootRoute();
  const index = createRoute({ getParentRoute: () => root, path: "/", component: () => <>{element}</> });
  const detail = createRoute({
    getParentRoute: () => root,
    path: "/task-runs/$taskRunId",
    component: () => <div>task</div>,
  });
  const router = createRouter({
    routeTree: root.addChildren([index, detail]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

test("cron preview reads a human description", () => {
  expect(cronDescription("0 9 * * 1-5")).toBe("At 09:00, Monday through Friday");
  expect(cronDescription("not a cron")).toBeNull();
  expect(
    draftToBody({
      kind: "cron",
      cron: "0 9 * * *",
      timezone: "local",
      day_or: true,
      catchup: "skip",
      catchup_max: 100,
    }),
  ).toMatchObject({ kind: "cron", timezone: null });
});

test("schedule editor shows the preview and inline validation", async () => {
  const onSave = vi.fn();
  const preview = vi.fn(async () => ({ next: [1, 2, 3] }));
  render(<ScheduleEditor onSave={onSave} preview={preview} />);
  const input = screen.getByLabelText("Cron expression") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "0 9 * * 1-5" } });
  expect(screen.getByTestId("cron-preview").textContent).toBe("At 09:00, Monday through Friday");
  await waitFor(() => expect(screen.getByTestId("next-fires").textContent).toContain("Next:"));
  fireEvent.change(input, { target: { value: "bad" } });
  expect(screen.getByTestId("cron-preview").textContent).toContain("Invalid");
  fireEvent.click(screen.getByText("Save schedule"));
  expect(onSave).not.toHaveBeenCalled();
});

test("run form renders a date picker and submits an ISO date", () => {
  const schema = {
    type: "object",
    properties: {
      day: { type: "string", format: "date" },
      n: { type: "integer", default: 1 },
      flag: { type: "boolean", default: false },
    },
    required: ["day"],
  };
  const onSubmit = vi.fn();
  render(<RunForm schema={schema} onSubmit={onSubmit} />);
  const day = screen.getByLabelText("day *") as HTMLInputElement;
  expect(day.type).toBe("date");
  fireEvent.click(screen.getByText("Run"));
  expect(onSubmit).not.toHaveBeenCalled();
  expect(screen.getByText("day is required")).toBeInTheDocument();
  fireEvent.change(day, { target: { value: "2026-09-06" } });
  fireEvent.change(screen.getByLabelText("n"), { target: { value: "abc" } });
  expect(screen.getByText("must be an integer")).toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("n"), { target: { value: "4" } });
  fireEvent.click(screen.getByText("Run"));
  expect(onSubmit).toHaveBeenCalledWith({ day: "2026-09-06", n: 4, flag: false });
  expect(defaultsFromSchema(schema)).toEqual({ day: null, n: 1, flag: false });
  expect(coerceValue("datetime", "2026-09-06T10:00")).toEqual({ value: "2026-09-06T10:00:00" });
});

test("backfill helpers count runs", () => {
  expect(intervalSeconds("1d")).toBe(86400);
  expect(intervalSeconds("12h")).toBe(43200);
  expect(intervalSeconds("nope")).toBeNull();
  expect(countRuns("2026-06-01", "2026-06-30", "1d")).toBe(30);
  expect(countRuns("2026-06-30", "2026-06-01", "1d")).toBeNull();
  expect(
    dateParameters({
      parameter_schema: { properties: { day: { type: "string", format: "date" }, n: { type: "integer" } } },
    } as any),
  ).toEqual(["day"]);
});

test("skipped badge is teal and awaiting resource names the resource", () => {
  render(
    <StateBadge state={{ type: "Completed", name: "Skipped", message: null, details: {}, timestamp: 0 }} />,
  );
  expect(screen.getByText("Skipped").className).toContain("teal");
  render(
    <StateBadge
      state={{
        type: "Scheduled",
        name: "AwaitingResource",
        message: "waiting for gpu",
        details: { resource: "gpu" } as any,
        timestamp: 0,
      }}
    />,
  );
  expect(screen.getByText("AwaitingResource")).toHaveAttribute("title", "waiting for gpu");
});

test("timeline draws bars and edges and selects a node", async () => {
  const graph = {
    run_id: 1,
    nodes: [
      {
        id: 1,
        external_id: "a",
        name: "a",
        dynamic_key: "a-0",
        state: { type: "Completed", name: "Completed", message: null, details: {}, timestamp: 0 },
        start_time: 1_000_000,
        end_time: 2_000_000,
        created_at: 1_000_000,
      },
      {
        id: 2,
        external_id: "b",
        name: "b",
        dynamic_key: "b-0",
        state: { type: "Failed", name: "Failed", message: null, details: {}, timestamp: 0 },
        start_time: 2_000_000,
        end_time: 3_000_000,
        created_at: 2_000_000,
      },
    ],
    edges: [{ from: 1, to: 2 }],
  } as any;
  expect(Object.fromEntries(dependencyLevels(graph.nodes, graph.edges))).toEqual({ 1: 0, 2: 1 });
  const { container } = withRouter(<Timeline graph={graph} now={3_000_000} />);
  await waitFor(() => expect(container.querySelectorAll("rect").length).toBe(2));
  expect(container.querySelectorAll("svg[role='img'] path").length).toBe(1);
  fireEvent.click(container.querySelector("[data-node='b-0']") as Element);
  expect(screen.getByTestId("timeline-panel")).toHaveTextContent("b-0");
  fireEvent.click(screen.getByText("dependency"));
  expect(container.querySelectorAll("rect").length).toBe(2);
});
