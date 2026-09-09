import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
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

const grouped = (id: number, name: string, project: string, group: string | null, type: string) =>
  ({ ...runs[0], id, name, project, group, state: { ...runs[0].state, type, name: type } }) as any;

test("grouped run table sections runs by group and rolls up their states", async () => {
  const rows = [
    grouped(1, "one", "warehouse", "nightly", "Completed"),
    grouped(2, "two", "analytics", "nightly", "Failed"),
    grouped(3, "three", "warehouse", null, "Completed"),
  ];
  // Swap the data from inside the tree: `rerender` would drop the router context.
  function Live() {
    const [data, setData] = useState(rows);
    return (
      <>
        <button
          type="button"
          onClick={() => setData((d) => [grouped(2, "two", "analytics", "nightly", "Completed"), d[0], d[2]])}
        >
          complete it
        </button>
        <RunTable runs={data} grouped />
      </>
    );
  }
  renderWithRouter(<Live />);
  await waitFor(() => expect(screen.getByTestId("group-nightly")).toBeInTheDocument());
  // A declared group spans projects; an undeclared run falls back to its project.
  expect(screen.getByTestId("group-nightly")).toHaveTextContent("analytics · warehouse");
  expect(screen.getByTestId("group-warehouse")).toBeInTheDocument();
  const nightly = screen.getByTestId("group-nightly");
  expect(nightly.querySelector("[data-testid=state-bar]")).toHaveAttribute(
    "aria-label",
    "1 Completed, 1 Failed",
  );

  // The rollup follows the rows: a state change is reflected in the header.
  screen.getByText("complete it").click();
  await waitFor(() =>
    expect(screen.getByTestId("group-nightly").querySelector("[data-testid=state-bar]")).toHaveAttribute(
      "aria-label",
      "2 Completed",
    ),
  );
});

test("selection works across groups", async () => {
  const rows = [
    grouped(1, "one", "warehouse", "nightly", "Completed"),
    grouped(2, "two", "warehouse", null, "Completed"),
  ];
  const picked = new Set<number>();
  renderWithRouter(
    <RunTable
      runs={rows}
      grouped
      selected={picked}
      onSelect={(id, checked) => (checked ? picked.add(id) : picked.delete(id))}
      onSelectAll={(checked) => {
        picked.clear();
        if (checked) for (const r of rows) picked.add(r.id);
      }}
    />,
  );
  await waitFor(() => expect(screen.getByText("one")).toBeInTheDocument());
  // Select-all spans every group, not just the first.
  screen.getByRole("checkbox", { name: "Select all runs on this page" }).click();
  expect(Array.from(picked).sort()).toEqual([1, 2]);
});

test("grouped run table: the group header spans exactly the table's columns", async () => {
  const rows = [grouped(1, "one", "warehouse", "nightly", "Completed")];
  renderWithRouter(
    <RunTable runs={rows} grouped selected={new Set()} onSelect={() => {}} onSelectAll={() => {}} />,
  );
  await waitFor(() => expect(screen.getByTestId("group-nightly")).toBeInTheDocument());
  const width = (tr: HTMLElement) =>
    Array.from(tr.querySelectorAll(":scope > td")).reduce(
      (n, td) => n + Number(td.getAttribute("colspan") ?? 1),
      0,
    );
  const dataRow = screen.getByText("one").closest("tr") as HTMLElement;
  const headings = document.querySelectorAll("thead th").length;
  expect(width(screen.getByTestId("group-nightly"))).toBe(width(dataRow));
  expect(width(screen.getByTestId("group-nightly"))).toBe(headings);
});
