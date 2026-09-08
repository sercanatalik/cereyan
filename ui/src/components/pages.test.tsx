import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { render, screen, waitFor } from "@testing-library/react";
import { Histogram } from "@/components/histogram";
import { RunTable } from "@/components/run-table";

const runs = [
  {
    id: 1,
    external_id: "x",
    flow_id: 1,
    flow_name: "etl",
    project: "proj",
    name: "brave-otter",
    parameters: {},
    tags: ["a"],
    state: { type: "Completed", name: "Completed", message: null, details: {}, timestamp: 1 },
    failure_count: 0,
    crash_count: 0,
    created_at: 1_000_000,
    start_time: 1_000_000,
    end_time: 2_000_000,
    total_run_time: 1_000_000,
    engine_pid: null,
    engine_id: null,
    created_by: "api",
    report_seq: 0,
  },
] as any[];

function renderWithRouter(element: React.ReactNode) {
  const root = createRootRoute();
  const index = createRoute({ getParentRoute: () => root, path: "/", component: () => <>{element}</> });
  const detail = createRoute({
    getParentRoute: () => root,
    path: "/runs/$runId",
    component: () => <div>detail</div>,
  });
  const router = createRouter({
    routeTree: root.addChildren([index, detail]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  const client = new QueryClient();
  return render(
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

test("run table renders rows with state badge and link", async () => {
  renderWithRouter(<RunTable runs={runs} />);
  await waitFor(() => expect(screen.getByText("brave-otter")).toBeInTheDocument());
  expect(screen.getByText("Completed")).toHaveAttribute("data-state", "Completed");
  expect(screen.getByText("proj/etl")).toBeInTheDocument();
  expect(screen.getByText("1.00 s")).toBeInTheDocument();
});

test("histogram buckets runs by state", () => {
  const { container } = render(<Histogram runs={runs} start={0} end={3_000_000} buckets={3} />);
  expect(container.querySelectorAll("rect").length).toBe(1);
});
