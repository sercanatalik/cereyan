import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { render, screen } from "@testing-library/react";
import { ArtifactCard } from "@/routes/artifacts";

const item = {
  id: 7,
  external_id: "x",
  run_id: 3,
  task_run_id: null,
  kind: "progress",
  key: "rows",
  data: { value: 40 },
  created_at: 1_000_000,
  updated_at: 2_000_000,
  run_name: "brave-otter",
  flow_name: "etl",
  project: "proj",
} as any;

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
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

test("artifact card links to its run and opens key history", async () => {
  const opened: string[] = [];
  renderWithRouter(<ArtifactCard item={item} onHistory={(k) => opened.push(k)} />);
  const link = await screen.findByTestId("artifact-run-7");
  expect(link).toHaveTextContent("proj/etl · brave-otter");
  expect(link).toHaveAttribute("href", "/runs/3");
  expect(screen.getByTestId("artifact-progress")).toHaveTextContent("40%");
  screen.getByRole("button", { name: "rows" }).click();
  expect(opened).toEqual(["rows"]);
});
