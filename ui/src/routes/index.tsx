import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { Filter, Play, RotateCcw } from "lucide-react";
import { useState } from "react";
import { api, type Run, type StateType, unwrap } from "@/api/client";
import { Histogram } from "@/components/histogram";
import { DateRangeSelect, type RangePreset, rangeStart } from "@/components/ported/date-range";
import { StateBadge, StateDot } from "@/components/ported/state-badge";
import { TagInput } from "@/components/ported/tag-input";
import { Page } from "@/components/shell";
import { StateBar, TaskProgress } from "@/components/state-bar";
import { Button } from "@/components/ui/button";
import { Card, CardHead } from "@/components/ui/card";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { useProject } from "@/lib/project";
import { formatDuration, relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/")({ component: Dashboard });

const ATTENTION: StateType[] = ["Paused", "Failed", "Crashed"];

function elapsed(run: Run, now: number): string {
  if (run.total_run_time != null) return formatDuration(run.total_run_time);
  if (run.start_time) return formatDuration(now - run.start_time);
  return relativeTime(run.created_at);
}

function scheduleWords(run: Run): string {
  if (run.created_by?.startsWith("schedule")) return "schedule";
  if (run.created_by?.startsWith("backfill")) return "backfill";
  return run.created_by ?? "";
}

function Dashboard() {
  const [range, setRange] = useState<RangePreset>("24h");
  const [tags, setTags] = useState<string[]>([]);
  const { project } = useProject();
  const start = rangeStart(range);
  const now = Date.now() * 1000;
  const counts = useQuery({
    queryKey: ["counts", project],
    queryFn: async () => unwrap(await api.GET("/api/counts", { params: { query: { project } } })),
  });
  const recent = useQuery({
    queryKey: ["runs", "dashboard", range, tags, project],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/runs", {
          params: {
            query: { limit: 500, start_after: start, tags: tags.join(",") || undefined, project },
          },
        }),
      ),
    refetchInterval: 10_000,
  });
  const upcoming = useQuery({
    queryKey: ["runs", "upcoming", project],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/runs", {
          params: {
            query: {
              state_type: "Scheduled",
              sort: "scheduled_asc",
              scheduled_after: Date.now() * 1000,
              limit: 5,
              project,
            },
          },
        }),
      ),
    refetchInterval: 15_000,
  });
  const runs = recent.data?.items ?? [];
  const byType = counts.data?.runs ?? {};
  const inRange = (t: StateType) => runs.filter((r) => r.state.type === t).length;
  const late = runs.filter((r) => r.state.name === "Late").length;
  const stats: { key: string; label: string; state: StateType | "Late"; value: number }[] = [
    { key: "Running", label: "Running", state: "Running", value: byType.Running ?? 0 },
    { key: "Completed", label: "Completed", state: "Completed", value: inRange("Completed") },
    { key: "Failed", label: "Failed", state: "Failed", value: inRange("Failed") },
    { key: "Crashed", label: "Crashed", state: "Crashed", value: inRange("Crashed") },
    { key: "Paused", label: "Waiting for input", state: "Paused", value: byType.Paused ?? 0 },
    { key: "Late", label: "Late", state: "Late", value: late },
    { key: "Scheduled", label: "Scheduled", state: "Scheduled", value: byType.Scheduled ?? 0 },
  ];
  const rank = (r: Run) => (r.state.name === "Late" ? ATTENTION.length : ATTENTION.indexOf(r.state.type));
  const attention = runs
    .filter((r) => ATTENTION.includes(r.state.type) || r.state.name === "Late")
    .sort((a, b) => rank(a) - rank(b))
    .slice(0, 6);
  const running = runs.filter((r) => r.state.type === "Running").slice(0, 6);
  const proportion: Record<string, number> = {};
  for (const r of runs) {
    const k = r.state.name === "Late" ? "Late" : r.state.type;
    proportion[k] = (proportion[k] ?? 0) + 1;
  }
  return (
    <Page
      title="Dashboard"
      actions={
        <>
          <DateRangeSelect value={range} onChange={setRange} />
          <TagInput
            value={tags}
            onChange={setTags}
            placeholder="Filter by tag"
            icon={<Filter className="size-3.5" />}
          />
        </>
      }
    >
      <Card className="gap-4 px-5 py-5">
        <div className="flex items-start justify-between">
          <div className="flex gap-9">
            {stats.map((s) => (
              <div key={s.key} className="flex min-w-24 flex-col gap-0.5" data-testid={`count-${s.key}`}>
                <span className="text-[26px] font-semibold leading-8 tracking-tight tabular-nums">
                  {s.value}
                </span>
                <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                  {s.state === "Late" ? (
                    <span className="inline-block h-[7px] w-[7px] rounded-full bg-orange-500" />
                  ) : (
                    <StateDot type={s.state} />
                  )}
                  {s.label}
                </span>
              </div>
            ))}
          </div>
          <div className="flex flex-col items-end gap-0.5">
            <span className="text-[26px] font-semibold leading-8 tracking-tight tabular-nums">
              {runs.length}
            </span>
            <span className="text-xs text-muted-foreground">runs in range</span>
          </div>
        </div>
        <StateBar counts={proportion} className="w-full" height={6} title={`${runs.length} runs by state`} />
        <Histogram runs={runs} start={start ?? runs.at(-1)?.created_at ?? now - 86_400_000_000} end={now} />
      </Card>

      <div className="grid grid-cols-[minmax(0,7fr)_minmax(0,5fr)] gap-4">
        <Card className="gap-0 py-0">
          <CardHead title="Needs attention" aside={`${attention.length} runs`} />
          {attention.length === 0 ? <Empty>Nothing waiting on you.</Empty> : null}
          {attention.map((r) => (
            <AttentionRow key={r.id} run={r} />
          ))}
        </Card>
        <Card className="gap-0 py-0">
          <CardHead title="Running now" aside={`${byType.Running ?? running.length} running`} />
          {running.length === 0 ? <Empty>Nothing running.</Empty> : null}
          {running.map((r) => (
            <div
              key={r.id}
              className="flex flex-col gap-2 border-b px-4 py-3 last:border-b-0"
              data-run-id={r.id}
            >
              <div className="flex items-baseline gap-2">
                <StateDot type="Running" />
                <Link
                  to="/runs/$runId"
                  params={{ runId: String(r.id) }}
                  className="font-medium hover:underline"
                >
                  {r.name}
                </Link>
                <span className="text-xs text-muted-foreground">
                  {r.project}/{r.flow_name}
                </span>
                <span className="ml-auto font-mono text-xs tabular-nums text-muted-foreground">
                  {elapsed(r, now)}
                </span>
              </div>
              <TaskProgress counts={r.task_counts ?? {}} />
            </div>
          ))}
        </Card>
      </div>

      <Card className="gap-0 py-0">
        <CardHead title="Upcoming" aside="next scheduled runs" />
        <Table>
          <thead>
            <tr>
              <Th className="w-40">When</Th>
              <Th>Flow</Th>
              <Th>Created by</Th>
              <Th>Parameters</Th>
              <Th className="w-28" />
            </tr>
          </thead>
          <tbody>
            {(upcoming.data?.items ?? []).map((r) => (
              <UpcomingRow key={r.id} run={r} />
            ))}
            {upcoming.data && upcoming.data.items.length === 0 ? (
              <tr>
                <td colSpan={5} className="px-4 py-6 text-center text-muted-foreground">
                  No schedules are due.
                </td>
              </tr>
            ) : null}
          </tbody>
        </Table>
      </Card>
    </Page>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return <div className="px-4 py-6 text-muted-foreground">{children}</div>;
}

function AttentionRow({ run }: { run: Run }) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const again = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/flows/{id}/runs", {
          params: { path: { id: run.flow_id } },
          body: { parameters: run.parameters as any, tags: [] },
        }),
      ),
    onSuccess: (created) => {
      client.invalidateQueries({ queryKey: ["runs"] });
      navigate({ to: "/runs/$runId", params: { runId: String(created.id) } });
    },
  });
  const details = (run.state.details ?? {}) as { prompt?: string };
  const message =
    run.state.type === "Paused"
      ? (details.prompt ?? run.state.message ?? "Waiting for input")
      : run.state.name === "Late"
        ? `Scheduled for ${new Date((run.scheduled_time ?? run.created_at) / 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`
        : (run.state.message ?? "");
  const open = () => navigate({ to: "/runs/$runId", params: { runId: String(run.id) } });
  return (
    <div
      className="grid grid-cols-[104px_minmax(0,1fr)_auto] items-center gap-3.5 border-b px-4 py-3 last:border-b-0"
      data-run-id={run.id}
      data-testid="attention-row"
    >
      <StateBadge state={run.state} />
      <div className="flex min-w-0 flex-col">
        <div className="flex items-baseline gap-2">
          <Link to="/runs/$runId" params={{ runId: String(run.id) }} className="font-medium hover:underline">
            {run.name}
          </Link>
          <span className="text-xs text-muted-foreground">
            {run.project}/{run.flow_name}
          </span>
          <span className="ml-auto text-xs text-muted-foreground">
            {relativeTime(run.state.timestamp || run.created_at)}
          </span>
        </div>
        <div className="truncate font-mono text-xs text-muted-foreground" title={message}>
          {message}
        </div>
      </div>
      {run.state.type === "Paused" ? (
        <Button size="sm" onClick={open} data-testid="answer-run">
          Answer
        </Button>
      ) : run.state.type === "Failed" ? (
        <Button size="sm" variant="outline" onClick={() => again.mutate()} disabled={again.isPending}>
          <RotateCcw /> Run again
        </Button>
      ) : (
        <Button size="sm" variant="outline" onClick={open}>
          Open
        </Button>
      )}
    </div>
  );
}

function UpcomingRow({ run }: { run: Run }) {
  const client = useQueryClient();
  const at = run.scheduled_time ?? run.created_at;
  const start = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/flows/{id}/runs", {
          params: { path: { id: run.flow_id } },
          body: { parameters: run.parameters as any, tags: run.tags ?? [] },
        }),
      ),
    onSuccess: () => client.invalidateQueries({ queryKey: ["runs"] }),
  });
  const params = Object.entries(run.parameters ?? {})
    .map(([k, v]) => `${k}=${typeof v === "string" ? v : JSON.stringify(v)}`)
    .join(", ");
  return (
    <Tr data-run-id={run.id}>
      <Td>
        <span className="tabular-nums">
          {new Date(at / 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}
        </span>
        <span className="ml-1.5 text-xs text-muted-foreground">{relativeTime(at)}</span>
      </Td>
      <Td>
        <Link
          to="/flows/$flowId"
          params={{ flowId: String(run.flow_id) }}
          className="font-medium hover:underline"
        >
          {run.project}/{run.flow_name}
        </Link>
      </Td>
      <Td className="text-muted-foreground">{scheduleWords(run)}</Td>
      <Td className="font-mono text-xs text-muted-foreground">{params}</Td>
      <Td className="text-right">
        <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => start.mutate()}>
          <Play /> Run now
        </Button>
      </Td>
    </Tr>
  );
}
