// The Settings tabs: the tab in the URL, the Environment tab's sources and
// hidden variables, and the Data tab's project removal and database reset.
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

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

const calls: { method: string; path: string; body: unknown }[] = [];
let projects: {
  name: string;
  flows: number;
  live_flows: number;
  runs: number;
  last_run_at: null;
  served: boolean;
}[] = [];

const settings = {
  host: "127.0.0.1",
  port: 4200,
  pid: 1,
  version: "1.14.0",
  title: "cereyan",
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
  secret_key_present: true,
  secret_key_missing: false,
  custom_routes: [],
  engine_saturation_risk: false,
  saturation_flows: [],
  saturation_reason: null,
};

const environment = {
  runtime: {
    python: "/home/ops/pipelines/.venv/bin/python",
    python_version: "3.12.4",
    platform: "linux x86_64",
    config_file: "/home/ops/pipelines/cereyan.toml",
  },
  configuration: [
    { table: "server", key: "port", value: 4200, source: "flag", source_name: "--port", secret: false },
    { table: "server", key: "token", value: null, source: "env", source_name: "CEREYAN_TOKEN", secret: true },
    { table: "defaults", key: "catchup", value: "skip", source: "default", source_name: null, secret: false },
    {
      table: "defaults",
      key: "crash_retries",
      value: 2,
      source: "settings",
      source_name: null,
      secret: false,
    },
  ],
  cereyan_toml: '# ops\n[server]\ntoken = "••••••••"\n',
  variables: [
    { name: "CEREYAN_HOME", value: "/home/ops/.cereyan", hidden: false },
    { name: "AWS_SECRET_ACCESS_KEY", value: null, hidden: true },
    { name: "HOME", value: "/home/ops", hidden: false },
  ],
  cereyan_unset: ["CEREYAN_PORT"],
};

const database = {
  path: "/home/ops/.cereyan/db.sqlite",
  bytes: 2048,
  wal_bytes: 0,
  counts: {
    runs: 12408,
    task_runs: 1,
    logs: 3,
    events: 4,
    artifacts: 0,
    variables: 9,
    ui_rules: 4,
    ui_schedules: 0,
  },
  stale_projects: 1,
  backup_dir: "/home/ops/.cereyan/backups",
};

const deleted = {
  flows: 2,
  runs: 1,
  task_runs: 0,
  logs: 0,
  events: 4,
  artifacts: 0,
  schedules: 0,
  backfills: 0,
  rules: 0,
  variables: 0,
};

beforeEach(() => {
  calls.length = 0;
  projects = [
    { name: "beta", flows: 2, live_flows: 2, runs: 3, last_run_at: null, served: true },
    { name: "alpha", flows: 2, live_flows: 0, runs: 1, last_run_at: null, served: false },
  ];
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const method = init?.method ?? request?.method ?? "GET";
    const p = url.pathname;
    const body = method === "POST" || method === "PATCH" ? await request?.json() : null;
    calls.push({ method, path: p, body });
    if (p === "/api/server")
      return json({
        version: "1.14.0",
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
      });
    if (p === "/api/settings") return json(settings);
    if (p === "/api/settings/environment") return json(environment);
    if (p === "/api/database/reset")
      return json({ scope: "everything", backup_path: "/home/ops/.cereyan/backups/db-1.sqlite", deleted });
    if (p === "/api/database") return json(database);
    if (p === "/api/projects") return json(projects);
    if (p === "/api/projects/alpha" && method === "DELETE") {
      projects = projects.filter((x) => x.name !== "alpha");
      return json(deleted);
    }
    if (p === "/api/projects/alpha")
      return json({
        name: "alpha",
        flows: 2,
        runs: 1,
        schedules: 0,
        backfills: 0,
        events: 4,
        matching_rules: 1,
        served: false,
        active_runs: 0,
      });
    if (p === "/api/flows")
      return json(
        projects.map((x, i) => ({ id: i + 1, project: x.name, name: `f${i}`, live: true, recent_runs: [] })),
      );
    return json({ error: `unmocked ${method} ${p}` }, 404);
  });
});

function mount(path: string, project?: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: [path] }),
    context: { queryClient: client },
  });
  render(
    <QueryClientProvider client={client}>
      <ProjectProvider initial={project}>
        <RouterProvider router={router} />
      </ProjectProvider>
    </QueryClientProvider>,
  );
  return router;
}

const button = async (name: string) => (await screen.findByRole("button", { name })) as HTMLButtonElement;

test("the selected tab lives in the URL", async () => {
  const router = mount("/settings?tab=environment");
  expect(await screen.findByText("Configuration")).toBeTruthy();
  fireEvent.click(screen.getByRole("tab", { name: "Data" }));
  await waitFor(() => expect(router.state.location.search).toEqual({ tab: "data" }));
  expect(await screen.findByText("Danger zone")).toBeTruthy();
});

test("turning Show defaults off leaves only settings something set", async () => {
  mount("/settings?tab=environment");
  await screen.findByTestId("config-defaults.catchup");
  expect(screen.getByTestId("config-server.port").textContent).toContain("--port");
  expect(screen.getByTestId("config-defaults.crash_retries").textContent).toContain("edited in Settings");
  expect(screen.getByTestId("config-server.token").textContent).toContain("••••••••");
  fireEvent.click(screen.getByRole("switch", { name: "Show defaults" }));
  await waitFor(() => expect(screen.queryByTestId("config-defaults.catchup")).toBeNull());
  expect(screen.getByTestId("config-server.port")).toBeTruthy();
});

test("hidden variables are masked and offer nothing to reveal", async () => {
  mount("/settings?tab=environment");
  const row = (await screen.findByText("AWS_SECRET_ACCESS_KEY")).closest("tr") as HTMLElement;
  expect(row.textContent).toContain("••••••••");
  expect(row.textContent).toContain("hidden");
  expect(row.querySelector("button")).toBeNull();
  expect(screen.getByText("CEREYAN_PORT")).toBeTruthy();
});

test("the served project cannot be removed", async () => {
  mount("/settings?tab=data");
  expect((await button("Remove beta")).disabled).toBe(true);
  expect((await button("Remove alpha")).disabled).toBe(false);
});

test("removing a project needs its name typed, and the switcher falls back to all projects", async () => {
  mount("/settings?tab=data", "alpha");
  await waitFor(() => expect(screen.getByTestId("shell")).toHaveAttribute("data-scope-project", "alpha"));
  fireEvent.click(await button("Remove alpha"));
  const remove = await button("Remove project");
  expect((await screen.findByTestId("removal-preview")).textContent).toContain("4");
  const confirm = screen.getByLabelText(/to confirm/);
  fireEvent.change(confirm, { target: { value: "alph" } });
  expect(remove.disabled).toBe(true);
  fireEvent.change(confirm, { target: { value: "alpha" } });
  await waitFor(() => expect(remove.disabled).toBe(false));
  fireEvent.click(remove);
  await waitFor(() =>
    expect(calls).toContainEqual({ method: "DELETE", path: "/api/projects/alpha", body: null }),
  );
  await waitFor(() => expect(screen.getByTestId("shell")).toHaveAttribute("data-scope-project", ""));
  await waitFor(() => expect(screen.queryByRole("button", { name: "Remove alpha" })).toBeNull());
});

test("the reset dialog defaults to the history, copies first, and needs reset typed", async () => {
  mount("/settings?tab=data");
  fireEvent.click(await button("Reset database…"));
  const history = await button("Delete run history");
  expect(history.disabled).toBe(true);
  expect((screen.getByRole("radio", { name: /Run history/ }) as HTMLInputElement).checked).toBe(true);
  expect(screen.getByRole("checkbox", { name: /Save a copy first/ }).getAttribute("data-state")).toBe(
    "checked",
  );
  fireEvent.change(screen.getByLabelText(/to confirm/), { target: { value: "reset" } });
  fireEvent.click(screen.getByRole("radio", { name: /Everything/ }));
  const everything = await button("Reset everything");
  expect(everything.disabled).toBe(false);
  fireEvent.click(everything);
  await waitFor(() =>
    expect(calls).toContainEqual({
      method: "POST",
      path: "/api/database/reset",
      body: { scope: "everything", backup: true },
    }),
  );
  expect((await screen.findByTestId("last-reset")).textContent).toContain("db-1.sqlite");
});
