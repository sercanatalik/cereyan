// Run retention and backup settings on the General tab, and the backup button on the Data tab.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ProjectProvider } from "@/lib/project";
import { routeTree } from "@/routeTree.gen";

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

const patches: unknown[] = [];
let backups = 0;

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

const settings = () => ({
  host: "127.0.0.1",
  port: 4200,
  pid: 1,
  version: "2.1.0",
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
  retain_runs_days: 14,
  retain_failed_runs_days: 60,
  keep_last_runs_per_flow: 10,
  backup_every: 24,
  backup_keep: 7,
  last_backup_at: null,
  last_backup_path: null,
  backups,
  email_configured: false,
  secret_key_present: false,
  secret_key_missing: false,
  custom_routes: [],
  engine_saturation_risk: false,
  saturation_flows: [],
  saturation_reason: null,
});

beforeEach(() => {
  patches.length = 0;
  backups = 0;
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const method = init?.method ?? request?.method ?? "GET";
    const p = url.pathname;
    if (p === "/api/server")
      return json({
        version: "2.1.0",
        home: "/h",
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
      });
    if (p === "/api/settings" && method === "PATCH") {
      patches.push(request ? await request.json() : JSON.parse(String(init?.body)));
      return json(settings());
    }
    if (p === "/api/settings") return json(settings());
    if (p === "/api/database" && method === "GET")
      return json({
        path: "/h/db.sqlite",
        bytes: 1024,
        wal_bytes: 0,
        counts: {
          runs: 3,
          task_runs: 4,
          logs: 5,
          events: 6,
          artifacts: 0,
          variables: 0,
          ui_rules: 0,
          ui_schedules: 0,
        },
        stale_projects: 0,
        backup_dir: "/h/backups",
        backups,
      });
    if (p === "/api/database/backup" && method === "POST") {
      backups += 1;
      return json({ path: "/h/backups/db-20260920-120000.sqlite", backups });
    }
    if (p === "/api/projects") return json([]);
    if (p === "/api/flows") return json([]);
    if (p === "/api/settings/environment")
      return json({
        runtime: { python: "python3", python_version: null, platform: null, config_file: null },
        configuration: [],
        cereyan_toml: null,
        variables: [],
        cereyan_unset: [],
      });
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

test("saving the defaults sends the run retention and backup settings", async () => {
  mount("/settings");
  const runs = (await screen.findByLabelText(/Retention days \(runs/)) as HTMLInputElement;
  await waitFor(() => expect(runs.value).toBe("14"));
  fireEvent.change(runs, { target: { value: "7" } });
  fireEvent.change(screen.getByLabelText(/Backup every/), { target: { value: "12" } });
  fireEvent.click(screen.getByRole("button", { name: "Save settings" }));
  await waitFor(() => expect(patches.length).toBe(1));
  expect(patches[0]).toMatchObject({
    retain_days: 30,
    retain_runs_days: 7,
    retain_failed_runs_days: 60,
    keep_last_runs_per_flow: 10,
    backup_every: 12,
    backup_keep: 7,
    crash_retries: 5,
  });
});

test("the Data tab backs up on demand and shows the copy", async () => {
  mount("/settings?tab=data");
  const button = await screen.findByTestId("backup-now");
  fireEvent.click(button);
  await waitFor(() => expect(screen.getByText(/db-20260920-120000\.sqlite/)).toBeTruthy());
  const posted = fetchMock.mock.calls.filter(
    ([i, init]) =>
      String(i instanceof Request ? i.url : i).includes("/api/database/backup") &&
      (i instanceof Request ? i.method : (init as RequestInit | undefined)?.method) === "POST",
  );
  expect(posted.length).toBe(1);
});
