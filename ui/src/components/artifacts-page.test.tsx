import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { render, screen } from "@testing-library/react";
import { ArtifactCard, ArtifactList } from "@/components/artifacts";

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

test("artifact list pages with Previous and Next", async () => {
  const calls: string[] = [];
  const page = (ids: number[], next: number | null) => ({
    items: ids.map((id) => ({ ...item, id, key: `k${id}` })),
    next_cursor: next,
  });
  fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
    const url = new URL(typeof input === "string" ? input : (input as Request).url, "http://localhost");
    calls.push(url.search);
    const body = url.searchParams.get("after") === "8" ? page([7, 6], null) : page([9, 8], 8);
    return new Response(JSON.stringify(body), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  });
  renderWithRouter(<ArtifactList filter={{ run_id: 3 }} pageSize={2} showRun={false} />);
  await screen.findByTestId("artifact-item-9");
  expect(screen.queryByTestId("artifact-run-9")).toBeNull();
  expect(calls[0]).toContain("run_id=3");
  expect(calls[0]).toContain("limit=2");
  expect(screen.getByTestId("page-footer")).toHaveTextContent("1–2 of more artifacts");
  expect(screen.getByRole("button", { name: "Previous" })).toBeDisabled();
  screen.getByRole("button", { name: "Next" }).click();
  await screen.findByTestId("artifact-item-7");
  expect(calls.at(-1)).toContain("after=8");
  expect(screen.getByTestId("page-footer")).toHaveTextContent("3–4 artifacts");
  expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
  screen.getByRole("button", { name: "Previous" }).click();
  await screen.findByTestId("artifact-item-9");
  expect(screen.getByTestId("page-footer")).toHaveTextContent("1–2 of more artifacts");
});
