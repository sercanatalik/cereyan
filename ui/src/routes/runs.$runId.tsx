import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { MoreHorizontal, RotateCcw } from "lucide-react";
import { useState } from "react";
import { api, type Run, unwrap } from "@/api/client";
import { ArtifactsTab } from "@/components/artifacts";
import { PausedBanner } from "@/components/paused-banner";
import { JsonView } from "@/components/ported/json-view";
import { KeyValueList } from "@/components/ported/key-value-list";
import { RunLogs } from "@/components/ported/run-logs";
import { StateBadge } from "@/components/ported/state-badge";
import { Tags } from "@/components/run-table";
import { Crumb, Page } from "@/components/shell";
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
import { formatDuration, formatTime } from "@/lib/utils";

type Search = { tab?: string; task?: number };

export const Route = createFileRoute("/runs/$runId")({
  validateSearch: (s: Record<string, unknown>): Search => ({
    tab: typeof s.tab === "string" ? s.tab : undefined,
    task:
      typeof s.task === "number"
        ? s.task
        : typeof s.task === "string"
          ? Number(s.task) || undefined
          : undefined,
  }),
  component: RunDetail,
});

const TERMINAL = new Set(["Completed", "Failed", "Cancelled", "Crashed"]);

function createdBy(run: Run): { label: string; to?: string; params?: Record<string, string> } {
  const by = run.created_by ?? "";
  if (by.startsWith("run:"))
    return { label: `run ${by.slice(4)}`, to: "/runs/$runId", params: { runId: by.slice(4) } };
  if (run.schedule_id != null)
    return { label: "schedule", to: "/flows/$flowId", params: { flowId: String(run.flow_id) } };
  if (run.backfill_id != null) return { label: "backfill" };
  return { label: by || "-" };
}

function RunDetail() {
  const { runId } = Route.useParams();
  const id = Number(runId);
  const navigate = useNavigate();
  const client = useQueryClient();
  const search = Route.useSearch();
  const [tab, setTab] = useState(search.tab ?? "logs");
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
  const graph = useQuery({
    queryKey: ["run-graph", id, tasks.data?.map((t) => `${t.id}:${t.state.name}`).join(",")],
    queryFn: async () => unwrap(await api.GET("/api/runs/{id}/graph", { params: { path: { id } } })),
    enabled: tab === "timeline",
  });
  const again = useMutation({
    mutationFn: async (r: Run) =>
      unwrap(
        await api.POST("/api/flows/{id}/runs", {
          params: { path: { id: r.flow_id } },
          body: { parameters: r.parameters as any, tags: [] },
        }),
      ),
    onSuccess: (created) => navigate({ to: "/runs/$runId", params: { runId: String(created.id) } }),
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
  if (!r)
    return (
      <Page crumbs={[{ label: "Runs", to: "/runs" }, { label: runId }]}>
        {run.isError ? "Run not found" : "Loading"}
      </Page>
    );
  const active = !TERMINAL.has(r.state.type);
  const by = createdBy(r);
  const selected = tasks.data?.find((t) => t.id === selectedTask);
  const params = Object.entries(r.parameters ?? {})
    .map(([k, v]) => `${k}=${typeof v === "string" ? v : JSON.stringify(v)}`)
    .join(", ");
  return (
    <Page wide className="gap-0">
      <div className="flex flex-col gap-3.5 border-b bg-card px-8 pt-4 pb-4" data-testid="run-header">
        <Crumb items={[{ label: "Runs", to: "/runs" }, { label: r.name }]} />
        <div className="flex flex-wrap items-center gap-3">
          <h1 className="text-[22px] font-semibold leading-7 tracking-tight">{r.name}</h1>
          <StateBadge state={r.state} />
          <Link
            to="/flows/$flowId"
            params={{ flowId: String(r.flow_id) }}
            className="text-muted-foreground hover:underline"
          >
            {r.project}/{r.flow_name}
          </Link>
          <Tags tags={r.tags} />
          <div className="ml-auto flex gap-2">
            <Button variant="outline" onClick={() => again.mutate(r)} disabled={again.isPending}>
              <RotateCcw /> Run again
            </Button>
            <Button
              variant="outline"
              disabled={!active || cancel.isPending}
              onClick={() => window.confirm("Cancel this run?") && cancel.mutate()}
            >
              Cancel
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="icon" aria-label="More actions">
                  <MoreHorizontal />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
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
        <div className="flex flex-wrap items-center gap-x-4.5 gap-y-1 text-xs text-muted-foreground">
          <span>
            Started{" "}
            <span className="text-foreground tabular-nums">{formatTime(r.start_time ?? r.created_at)}</span>
          </span>
          <span>
            {active ? "Elapsed" : "Duration"}{" "}
            <span className="text-foreground tabular-nums">
              {formatDuration(r.total_run_time ?? (r.start_time ? Date.now() * 1000 - r.start_time : null))}
            </span>
          </span>
          <span>
            Attempt <span className="text-foreground tabular-nums">{r.attempt ?? 0}</span>
          </span>
          <span>
            Created by{" "}
            {by.to ? (
              <Link to={by.to} params={by.params as any} className="text-foreground hover:underline">
                {by.label}
              </Link>
            ) : (
              <span className="text-foreground">{by.label}</span>
            )}
          </span>
          {params ? (
            <span>
              Parameters <span className="font-mono text-muted-foreground">{params}</span>
            </span>
          ) : null}
        </div>
        {r.state.type === "Paused" ? (
          <PausedBanner run={r} onResumed={() => client.invalidateQueries({ queryKey: ["run", id] })} />
        ) : r.state.message ? (
          <div
            className="rounded-md border bg-muted/50 px-3 py-2 font-mono text-xs"
            data-testid="state-message"
          >
            {r.state.message}
          </div>
        ) : null}
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-[320px_minmax(0,1fr)]">
        <TaskRail tasks={tasks.data ?? []} selectedId={selectedTask} onSelect={setSelectedTask} />
        <div className="flex min-w-0 flex-col">
          <UnderlineTabs
            className="px-6"
            items={[
              { value: "logs", label: "Logs" },
              { value: "timeline", label: "Timeline" },
              { value: "artifacts", label: "Artifacts" },
              { value: "parameters", label: "Parameters" },
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
