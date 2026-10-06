import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { CircleAlert, MoreHorizontal, RotateCcw } from "lucide-react";
import { useEffect, useState } from "react";
import { api, type Run, type TaskRun, unwrap } from "@/api/client";
import { ArtifactsTab } from "@/components/artifacts";
import { JsonView } from "@/components/json-view";
import { KeyValueList } from "@/components/key-value-list";
import { PausedBanner } from "@/components/paused-banner";
import { RunLogs, useLogLines } from "@/components/run-logs";
import { RunHost, Tags } from "@/components/run-table";
import { Crumb, Page } from "@/components/shell";
import { StateBadge } from "@/components/state-badge";
import { TaskRail } from "@/components/task-rail";
import { Timeline } from "@/components/timeline";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { UnderlineTabs } from "@/components/ui/underline-tabs";
import { runSummary } from "@/lib/summary/run";
import { formatDuration, formatTime } from "@/lib/utils";

type Search = { tab?: string; task?: number; pass?: number };

const asNumber = (v: unknown): number | undefined =>
  typeof v === "number" ? v : typeof v === "string" && v !== "" ? Number(v) : undefined;

export const Route = createFileRoute("/runs/$runId")({
  validateSearch: (s: Record<string, unknown>): Search => ({
    tab: typeof s.tab === "string" ? s.tab : undefined,
    task:
      typeof s.task === "number"
        ? s.task
        : typeof s.task === "string"
          ? Number(s.task) || undefined
          : undefined,
    pass: Number.isFinite(asNumber(s.pass)) ? asNumber(s.pass) : undefined,
  }),
  component: RunDetail,
});

const TERMINAL = new Set(["Completed", "Failed", "Cancelled", "Crashed"]);
const RETRIABLE = new Set(["Failed", "Cancelled", "Crashed"]);
/** The page size of a run's Artifacts tab, so its first page doubles as the count. */
const ARTIFACT_PAGE = 20;

/** What started the run, linking the schedule's flow or the run before it. */
function triggeredBy(run: Run): { label: string; to?: string; params?: Record<string, string> } {
  const by = run.created_by ?? "";
  if (by.startsWith("run:"))
    return { label: `run ${by.slice(4)}`, to: "/runs/$runId", params: { runId: by.slice(4) } };
  if (run.parent_run_id != null)
    return {
      label: `${by.startsWith("crash:") ? "crash" : "retry"} of run ${run.parent_run_id}`,
      to: "/runs/$runId",
      params: { runId: String(run.parent_run_id) },
    };
  if (run.schedule_id != null)
    return { label: "schedule", to: "/flows/$flowId", params: { flowId: String(run.flow_id) } };
  if (run.backfill_id != null) return { label: "backfill" };
  if (by.startsWith("user:")) return { label: by.slice(5) };
  return { label: by || "-" };
}

/** Wall-clock milliseconds, ticking each second while `live`. */
function useNow(live: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [live]);
  return now;
}

/** The task run a Failed or Crashed run failed on: the first to fail in the pass on show. */
function failingTask(tasks: TaskRun[]): TaskRun | undefined {
  return tasks
    .filter((t) => t.state.type === "Failed" || t.state.type === "Crashed")
    .sort((a, b) => (a.end_time ?? a.state.timestamp) - (b.end_time ?? b.state.timestamp))[0];
}

function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex min-w-0 flex-[1_1_150px] flex-col gap-0.5 bg-card px-4 py-2.5">
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="min-w-0 truncate font-medium">{children}</span>
    </div>
  );
}

function ErrorCard({
  task,
  message,
  crashed,
  onShow,
}: {
  task?: string;
  message?: string | null;
  crashed: boolean;
  onShow: () => void;
}) {
  return (
    <div
      role="alert"
      className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-red-900 dark:border-red-900/60 dark:bg-red-950/40 dark:text-red-200"
      data-testid="error-card"
    >
      <CircleAlert className="size-4.5 shrink-0" />
      <div className="flex min-w-0 flex-[1_1_320px] flex-col gap-0.5">
        <span className="font-semibold">
          {task ? (
            <>
              <span className="font-mono">{task}</span> {crashed ? "crashed" : "raised an error"}
            </>
          ) : crashed ? (
            "The run crashed"
          ) : (
            "The run failed"
          )}
        </span>
        {message ? (
          <code className="font-mono text-xs break-words whitespace-pre-wrap" data-testid="state-message">
            {message}
          </code>
        ) : null}
      </div>
      <button type="button" className="cursor-pointer font-medium underline" onClick={onShow}>
        Show in logs
      </button>
    </div>
  );
}

function RunDetail() {
  const { runId } = Route.useParams();
  const id = Number(runId);
  const navigate = useNavigate();
  const client = useQueryClient();
  const search = Route.useSearch();
  const [tab, setTab] = useState(search.tab ?? "logs");
  // Bumped by the error card's Show in logs; the Logs tab scrolls on each change.
  const [focusError, setFocusError] = useState<number | undefined>(undefined);
  const selectedTask = search.task;
  const setSelectedTask = (task: number | undefined) =>
    navigate({
      to: "/runs/$runId",
      params: { runId },
      search: (old: Search) => ({ ...old, task }),
      replace: true,
    });
  const run = useQuery({
    queryKey: ["run", id],
    queryFn: async () => unwrap(await api.GET("/api/runs/{id}", { params: { path: { id } } })),
  });
  const tasks = useQuery({
    queryKey: ["run-tasks", id],
    queryFn: async () => unwrap(await api.GET("/api/runs/{id}/tasks", { params: { path: { id } } })),
  });
  // A run's body can execute more than once; each execution is a pass. The
  // tasks request carries every pass, so the switcher knows what exists
  // without a second round trip.
  const passes = [...new Set((tasks.data ?? []).map((t) => t.pass ?? 0))].sort((a, b) => a - b);
  const latestPass = passes.length ? passes[passes.length - 1] : 0;
  const activePass = search.pass != null && passes.includes(search.pass) ? search.pass : latestPass;
  const visibleTasks = (tasks.data ?? []).filter((t) => (t.pass ?? 0) === activePass);
  const latestTasks = (tasks.data ?? []).filter((t) => (t.pass ?? 0) === latestPass);
  const setPass = (pass: number | undefined) =>
    navigate({
      to: "/runs/$runId",
      params: { runId },
      search: (old: Search) => ({ ...old, pass, task: undefined }),
      replace: true,
    });
  const graph = useQuery({
    queryKey: ["run-graph", id, activePass, visibleTasks.map((t) => `${t.id}:${t.state.name}`).join(",")],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/runs/{id}/graph", {
          params: { path: { id }, query: { pass: activePass } },
        }),
      ),
    enabled: tab === "timeline",
  });
  // The counts on the tab labels share their tabs' caches.
  const logLines = useLogLines(id, undefined);
  const artifacts = useQuery({
    queryKey: ["artifacts", "list", { run_id: id }, undefined, ARTIFACT_PAGE],
    queryFn: async () =>
      unwrap(await api.GET("/api/artifacts", { params: { query: { run_id: id, limit: ARTIFACT_PAGE } } })),
  });
  const again = useMutation({
    mutationFn: async (r: Run) =>
      unwrap(
        await api.POST("/api/flows/{id}/runs", {
          params: { path: { id: r.flow_id } },
          body: { parameters: r.parameters as any, tags: [] },
        }),
      ),
    onSuccess: (created) => {
      const target = "conflict" in created ? created.run : created;
      navigate({ to: "/runs/$runId", params: { runId: String(target.id) } });
    },
  });
  const retry = useMutation({
    mutationFn: async (from: string) =>
      unwrap(await api.POST("/api/runs/{id}/retry", { params: { path: { id } }, body: { from } })),
    onSuccess: (out) => navigate({ to: "/runs/$runId", params: { runId: String(out.run.id) } }),
  });
  const cancel = useMutation({
    mutationFn: async () => unwrap(await api.POST("/api/runs/{id}/cancel", { params: { path: { id } } })),
    onSuccess: (r) => client.setQueryData(["run", id], r),
  });
  const remove = useMutation({
    mutationFn: async () => api.DELETE("/api/runs/{id}", { params: { path: { id } } }),
    onSuccess: () => navigate({ to: "/runs" }),
  });
  const r = run.data;
  const active = !!r && !TERMINAL.has(r.state.type);
  const now = useNow(active);
  // The most recent earlier run of the same flow, for "Compare with previous run".
  const earlier = useQuery({
    queryKey: ["previous-run", id, r?.flow_id],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/runs", {
          params: { query: { flow: r?.flow_name, project: r?.project, limit: 20 } },
        }),
      ),
    enabled: !!r,
  });
  const previous = earlier.data?.items.find((x) => x.id < id) ?? null;
  // Notes tasks kept for their own later attempts (task_state).
  const notes = useQuery({
    queryKey: ["run-state", id],
    queryFn: async () => unwrap(await api.GET("/api/runs/{id}/state", { params: { path: { id } } })),
    enabled: !!r,
  });
  if (!r)
    return (
      <Page crumbs={[{ label: "Runs", to: "/runs" }, { label: runId }]}>
        {run.isError ? "Run not found" : "Loading"}
      </Page>
    );
  const by = triggeredBy(r);
  const selected = tasks.data?.find((t) => t.id === selectedTask);
  const params = Object.entries(r.parameters ?? {})
    .map(([k, v]) => `${k}=${typeof v === "string" ? v : JSON.stringify(v)}`)
    .join(", ");
  // The total once finished; while live, the time since it started.
  const duration = r.total_run_time ?? (r.start_time ? (r.end_time ?? now * 1000) - r.start_time : null);
  const summary = runSummary({
    state: r.state,
    duration,
    attempt: r.attempt,
    tasks: {
      completed: latestTasks.filter((t) => t.state.type === "Completed").length,
      total: latestTasks.length,
    },
  });
  const failed = r.state.type === "Failed" || r.state.type === "Crashed";
  const failing = failed ? failingTask(latestTasks) : undefined;
  const showInLogs = () => {
    // The failing task run is in the latest pass, so leave any earlier one.
    navigate({
      to: "/runs/$runId",
      params: { runId },
      search: (old: Search) => ({ ...old, pass: undefined, task: failing?.id }),
      replace: true,
    });
    setTab("logs");
    setFocusError((n) => (n ?? 0) + 1);
  };
  const taskKeys = Object.fromEntries((tasks.data ?? []).map((t) => [t.id, t.dynamic_key]));
  const paramCount = Object.keys(r.parameters ?? {}).length;
  const artifactCount = artifacts.data
    ? `${artifacts.data.items.length}${artifacts.data.next_cursor ? "+" : ""}`
    : undefined;
  const label = (text: string, count?: number | string) =>
    count == null ? (
      text
    ) : (
      <>
        {text}
        <span className="ml-1.5 text-xs font-normal text-muted-foreground tabular-nums">{count}</span>
      </>
    );
  return (
    <Page width="bleed" className="gap-0">
      <div className="border-b bg-card pt-4 pb-4" data-testid="run-header">
        <div className="frame flex flex-col gap-3.5">
          <Crumb
            items={[
              { label: "Runs", to: "/runs" },
              { label: `${r.project}/${r.flow_name}`, to: `/flows/${r.flow_id}` },
              { label: r.name },
            ]}
          />
          <div className="flex flex-wrap items-start justify-between gap-4">
            <div className="flex min-w-0 flex-col gap-1.5">
              <div className="flex flex-wrap items-center gap-3">
                <h1 className="text-[22px] font-semibold leading-7 tracking-tight">{r.name}</h1>
                <StateBadge state={r.state} />
                <Tags tags={r.tags} />
              </div>
              <p className="text-muted-foreground" data-testid="run-summary">
                {summary}
              </p>
            </div>
            <div className="flex flex-wrap gap-2">
              {RETRIABLE.has(r.state.type) ? (
                <Button
                  variant="outline"
                  onClick={() => retry.mutate("failure")}
                  disabled={retry.isPending}
                  data-testid="retry-from-failure"
                >
                  <RotateCcw /> Retry from failure
                </Button>
              ) : null}
              {active ? (
                <Button
                  variant="outline"
                  disabled={cancel.isPending || r.state.type === "Cancelling"}
                  onClick={() => window.confirm("Cancel this run?") && cancel.mutate()}
                >
                  Cancel
                </Button>
              ) : null}
              <Button onClick={() => again.mutate(r)} disabled={again.isPending}>
                <RotateCcw /> Run again
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="outline" size="icon" aria-label="More actions">
                    <MoreHorizontal />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem
                    disabled={!previous}
                    onSelect={() =>
                      previous && navigate({ to: "/runs/compare", search: { ids: `${previous.id},${r.id}` } })
                    }
                    data-testid="compare-previous"
                  >
                    {previous ? "Compare with previous run" : "No earlier run to compare"}
                  </DropdownMenuItem>
                  {active ? (
                    <DropdownMenuItem
                      disabled={cancel.isPending || r.state.type === "Cancelling"}
                      onSelect={() => window.confirm("Cancel this run?") && cancel.mutate()}
                    >
                      Cancel run
                    </DropdownMenuItem>
                  ) : null}
                  <DropdownMenuItem
                    variant="destructive"
                    onSelect={() => window.confirm("Delete this run and its history?") && remove.mutate()}
                  >
                    Delete run
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>
          <div
            className="flex flex-wrap gap-px overflow-hidden rounded-lg border bg-border"
            data-testid="run-facts"
          >
            <Fact label="Started">
              <span className="tabular-nums">{formatTime(r.start_time)}</span>
            </Fact>
            <Fact label={active ? "Elapsed" : "Duration"}>
              <span className="tabular-nums">{formatDuration(duration)}</span>
            </Fact>
            <Fact label="Attempts">
              <span className="tabular-nums">{(r.attempt ?? 0) + 1}</span>
            </Fact>
            <Fact label="Triggered by">
              {by.to ? (
                <Link to={by.to} params={by.params as any} className="hover:underline">
                  {by.label}
                </Link>
              ) : (
                by.label
              )}
            </Fact>
            <Fact label="Ran on">{r.host ? <RunHost run={r} className="px-0" /> : "-"}</Fact>
            <Fact label="Parameters">
              <span className="font-mono text-xs font-normal" title={params || undefined}>
                {params || "-"}
              </span>
            </Fact>
          </div>
          {r.state.type === "Paused" ? (
            <PausedBanner run={r} onResumed={() => client.invalidateQueries({ queryKey: ["run", id] })} />
          ) : failed ? (
            <ErrorCard
              task={failing?.dynamic_key}
              message={r.state.message}
              crashed={r.state.type === "Crashed"}
              onShow={showInLogs}
            />
          ) : r.state.message ? (
            <div
              className="rounded-md border bg-muted/50 px-3 py-2 font-mono text-xs"
              data-testid="state-message"
            >
              {r.state.message}
            </div>
          ) : null}
        </div>
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-[320px_minmax(0,1fr)]">
        <div className="flex min-h-0 flex-col">
          {passes.length > 1 ? (
            <div
              className="flex items-center gap-1.5 border-r border-b px-4 py-2"
              data-testid="pass-switcher"
            >
              <span className="text-xs text-muted-foreground">Pass</span>
              {passes.map((p) => (
                <Button
                  key={p}
                  type="button"
                  size="sm"
                  variant={p === activePass ? "secondary" : "ghost"}
                  className="h-6 px-2 text-xs tabular-nums"
                  onClick={() => setPass(p === latestPass ? undefined : p)}
                >
                  {p}
                </Button>
              ))}
            </div>
          ) : null}
          <TaskRail
            tasks={visibleTasks}
            selectedId={selectedTask}
            onSelect={setSelectedTask}
            onRerunFrom={active ? undefined : (t) => retry.mutate(t.dynamic_key)}
            className="min-h-0 flex-1"
          />
        </div>
        <div className="flex min-w-0 flex-col">
          <UnderlineTabs
            className="px-6"
            items={[
              { value: "logs", label: label("Logs", logLines.data?.length) },
              { value: "timeline", label: label("Timeline", visibleTasks.length || undefined) },
              { value: "artifacts", label: label("Artifacts", artifactCount) },
              { value: "parameters", label: label("Parameters", paramCount || undefined) },
              { value: "details", label: "Details" },
            ]}
            value={tab}
            onChange={setTab}
          />
          <div className="flex min-h-0 flex-1 flex-col gap-4 p-6">
            {tab === "logs" ? (
              <RunLogs
                runId={id}
                taskRunId={selected?.id}
                taskLabel={selected?.dynamic_key}
                onClearTask={() => setSelectedTask(undefined)}
                active={active}
                taskKeys={taskKeys}
                focusError={focusError}
              />
            ) : null}
            {tab === "timeline" ? (
              graph.data ? (
                <Timeline graph={graph.data} />
              ) : (
                <div className="text-muted-foreground">Loading</div>
              )
            ) : null}
            {tab === "artifacts" ? <ArtifactsTab runId={id} /> : null}
            {tab === "parameters" ? <JsonView value={r.parameters} /> : null}
            {tab === "details" ? (
              <KeyValueList
                items={[
                  ...((Array.isArray(notes.data) ? notes.data : []).map((n) => ({
                    label: `State ${n.scope ? `${n.scope}.` : ""}${n.key}`,
                    value: <code className="font-mono text-xs">{JSON.stringify(n.value)}</code>,
                  })) as { label: string; value: React.ReactNode }[]),
                  { label: "Id", value: String(r.id) },
                  { label: "External id", value: r.external_id },
                  { label: "Project", value: r.project },
                  { label: "Created", value: formatTime(r.created_at) },
                  {
                    label: "Created by",
                    value: r.created_by?.startsWith("run:") ? (
                      <Link to="/runs/$runId" params={{ runId: r.created_by.slice(4) }} className="underline">
                        run {r.created_by.slice(4)}
                      </Link>
                    ) : (
                      r.created_by
                    ),
                  },
                  { label: "Scheduled for", value: formatTime(r.scheduled_time) },
                  { label: "Priority", value: String(r.priority ?? 0) },
                  { label: "Attempt", value: String(r.attempt ?? 0) },
                  {
                    label: "Previous attempt",
                    value: r.parent_run_id ? (
                      <Link
                        to="/runs/$runId"
                        params={{ runId: String(r.parent_run_id) }}
                        className="underline"
                      >
                        run {r.parent_run_id}
                      </Link>
                    ) : (
                      "-"
                    ),
                  },
                  { label: "Failures", value: String(r.failure_count) },
                  { label: "Crashes", value: String(r.crash_count) },
                  {
                    label: "Host",
                    value: r.host
                      ? `${r.host}${r.processor != null ? ` · processor ${r.processor}` : ""}`
                      : "-",
                  },
                  { label: "Engine PID", value: r.engine_pid != null ? String(r.engine_pid) : "-" },
                ]}
              />
            ) : null}
          </div>
        </div>
      </div>
    </Page>
  );
}
