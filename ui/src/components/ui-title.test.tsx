// The configurable UI title in the top bar, the browser tab, and the Settings page.
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

let title = "cereyan";
const patches: unknown[] = [];

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

const settings = () => ({
  host: "127.0.0.1",
  port: 4200,
  pid: 1,
  version: "1.14.0",
  title,
  home: "/home/ops/.cereyan",
  served_dir: "/home/ops/pipelines",
  database_path: "/home/ops/.cereyan/db.sqlite",
  database_bytes: 1024,
  wal_bytes: 0,
  resources: {},
  max_engines: 4,
  engine_max_runs: 100,
  crash_retries_default: 5,
  catchup_default: "skip",
  retain_days: 30,
  email_configured: false,
  secret_key_present: false,
  secret_key_missing: false,
  custom_routes: [],
  engine_saturation_risk: false,
  saturation_flows: [],
  saturation_reason: null,
});

beforeEach(() => {
  title = "cereyan";
  patches.length = 0;
  document.title = "";
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const method = init?.method ?? request?.method ?? "GET";
    const p = url.pathname;
    if (p === "/api/server")
      return json({
        version: "1.14.0",
        home: "/home/ops/.cereyan",
        pid: 1,
        started_at: 0,
        url: "http://127.0.0.1:4200",
        base_path: "",
        title,
        served_dir: null,
        engines: [],
        queued: 0,
        stream_seq: 0,
      });
    if (p === "/api/settings" && method === "PATCH") {
      const body = request ? await request.json() : JSON.parse(String(init?.body));
      patches.push(body);
      title = String(body.title ?? "").trim() || "cereyan";
      return json(settings());
    }
    if (p === "/api/settings") return json(settings());
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
  return render(
    <QueryClientProvider client={client}>
      <ProjectProvider>
        <RouterProvider router={router} />
      </ProjectProvider>
    </QueryClientProvider>,
  );
}

const shown = () => screen.findByTestId("ui-title");

test("the top bar and the tab read cereyan when no title is set", async () => {
  mount("/settings");
  await waitFor(async () => expect((await shown()).textContent).toBe("cereyan"));
  expect(document.title).toBe("cereyan");
});

test("a configured title replaces cereyan in the top bar and the tab", async () => {
  title = "Data Platform";
  mount("/settings");
  await waitFor(async () => expect((await shown()).textContent).toBe("Data Platform"));
  expect(document.title).toBe("Data Platform");
});

test("a long title is capped with an ellipsis and keeps the full title as its tooltip", async () => {
  title = "Payments Data Platform — Production EU";
  mount("/settings");
  const el = await shown();
  await waitFor(() => expect(el.textContent).toBe(title));
  expect(el.className).toContain("max-w-60");
  expect(el.className).toContain("truncate");
  expect(el.getAttribute("title")).toBe(title);
});

test("saving a title sends only the title and updates the top bar without a reload", async () => {
  mount("/settings");
  // Loaded settings reset the field, so type only once they are in.
  await screen.findByDisplayValue("30");
  const input = (await screen.findByLabelText("Title")) as HTMLInputElement;
  fireEvent.change(input, { target: { value: "Ops" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(patches).toEqual([{ title: "Ops" }]));
  await waitFor(async () => expect((await shown()).textContent).toBe("Ops"));
  expect(document.title).toBe("Ops");
});

test("clearing the title restores cereyan", async () => {
  title = "Ops";
  mount("/settings");
  const input = (await screen.findByLabelText("Title")) as HTMLInputElement;
  await waitFor(() => expect(input.value).toBe("Ops"));
  fireEvent.change(input, { target: { value: "" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(patches).toEqual([{ title: "" }]));
  await waitFor(async () => expect((await shown()).textContent).toBe("cereyan"));
});
