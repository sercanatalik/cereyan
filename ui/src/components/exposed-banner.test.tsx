// The banner shown while the server is bound beyond loopback with no token required.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { render, screen, waitFor } from "@testing-library/react";
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

let exposed = false;

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  exposed = false;
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const method = init?.method ?? request?.method ?? "GET";
    const p = url.pathname;
    if (p === "/api/server")
      return json({
        version: "2.1.0",
        home: "/home/ops/.cereyan",
        pid: 1,
        started_at: 0,
        url: "http://127.0.0.1:4200",
        base_path: "",
        title: "cereyan",
        served_dir: null,
        engines: [],
        queued: 0,
        stream_seq: 0,
        auth: false,
        exposed,
        token_file: null,
      });
    if (p === "/api/rules") return json([]);
    if (p === "/api/flows") return json([]);
    return json({ error: `unmocked ${method} ${p}` }, 404);
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
  return client;
}

test("an exposed server shows the banner on every page with a link to the guide", async () => {
  exposed = true;
  mount("/rules");
  const banner = await screen.findByTestId("exposed-banner");
  expect(banner).toHaveTextContent("reachable from the network without a token");
  const link = banner.querySelector("a");
  expect(link?.getAttribute("href")).toContain("secure-the-server");
});

test("a loopback or token-protected server shows no banner", async () => {
  mount("/rules");
  await screen.findByTestId("shell");
  await waitFor(() =>
    expect(
      fetchMock.mock.calls.some(([i]) => String(i instanceof Request ? i.url : i).includes("/api/server")),
    ).toBe(true),
  );
  expect(screen.queryByTestId("exposed-banner")).toBeNull();
});

test("the banner goes away once the server reports it is no longer exposed", async () => {
  exposed = true;
  const client = mount("/rules");
  await screen.findByTestId("exposed-banner");
  exposed = false;
  await client.refetchQueries({ queryKey: ["server"] });
  await waitFor(() => expect(screen.queryByTestId("exposed-banner")).toBeNull());
});
