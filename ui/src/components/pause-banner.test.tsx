// The banner shown while every schedule is paused, and its Resume button.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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

let paused: null | { since: number; reason: string | null; until: number | null; suppress_rules: boolean } =
  null;
const calls: { method: string; path: string }[] = [];

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  paused = null;
  calls.length = 0;
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const method = init?.method ?? request?.method ?? "GET";
    const p = url.pathname;
    calls.push({ method, path: p });
    if (p === "/api/server")
      return json({
        version: "2.2.0",
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
        exposed: false,
        token_file: null,
        paused,
      });
    if (p === "/api/scheduler/resume") {
      paused = null;
      return json({ paused: false, since: null, reason: null, until: null, suppress_rules: false, held: 0 });
    }
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

test("a paused scheduler shows the reason and the end on every page", async () => {
  paused = {
    since: 1_758_330_000_000_000,
    reason: "db upgrade",
    until: 1_758_340_800_000_000,
    suppress_rules: true,
  };
  mount("/rules");
  const banner = await screen.findByTestId("pause-banner");
  expect(banner).toHaveTextContent("Scheduler paused: db upgrade");
  expect(banner).toHaveTextContent("until");
  expect(banner).toHaveTextContent("rule actions are suppressed");
});

test("a running scheduler shows no banner", async () => {
  mount("/rules");
  await screen.findByTestId("shell");
  await waitFor(() => expect(calls.some((c) => c.path === "/api/server")).toBe(true));
  expect(screen.queryByTestId("pause-banner")).toBeNull();
});

test("Resume calls the API and the banner goes away", async () => {
  paused = { since: 1_758_330_000_000_000, reason: null, until: null, suppress_rules: false };
  mount("/rules");
  const banner = await screen.findByTestId("pause-banner");
  expect(banner).toHaveTextContent("until resumed");
  fireEvent.click(screen.getByTestId("resume-scheduler"));
  await waitFor(() =>
    expect(calls.some((c) => c.method === "POST" && c.path === "/api/scheduler/resume")).toBe(true),
  );
  await waitFor(() => expect(screen.queryByTestId("pause-banner")).toBeNull());
});
