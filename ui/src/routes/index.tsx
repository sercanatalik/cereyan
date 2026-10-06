import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { Filter, Pause, RotateCcw } from "lucide-react";
import { useRef, useState } from "react";
import { api, type Flow, type Run, type StateType, unwrap } from "@/api/client";
import { DateRangeSelect, type RangePreset, rangeStart } from "@/components/date-range";
import { Histogram } from "@/components/histogram";
import { LogPreview } from "@/components/log-preview";
import { type RecentRun, RunStrip, RunStripLegend } from "@/components/run-strip";
import { Page } from "@/components/shell";
import { StateDot } from "@/components/state-badge";
import { TaskProgress } from "@/components/state-bar";
import { TagInput } from "@/components/tag-input";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { groupOf } from "@/lib/groups";
import { useMinWidth } from "@/lib/media";
import { useProject } from "@/lib/project";
import {
  dashboardFlows,
  defaultPreviewRun,
  flowStatusLine,
  idleProcessors,
  upcomingReason,
} from "@/lib/summary/dashboard";
import { cn, formatClock, formatDuration, formatIn, relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/")({ component: Dashboard });

const ATTENTION: StateType[] = ["Paused", "Failed", "Crashed"];
/** From this viewport width the histogram doubles its buckets. */
const WIDE = 1680;
const COMPLETED_LIMIT = 8;
const ATTENTION_LIMIT = 5;
const UPCOMING_LIMIT = 3;

function elapsed(run: Run, now: number): string {
  if (run.total_run_time != null) return formatDuration(run.total_run_time);
  if (run.start_time) return formatDuration(now - run.start_time);
  return relativeTime(run.created_at);
}

const finishedAt = (run: Run) => run.end_time ?? run.state.timestamp;
const isLate = (run: Run) => run.state.name === "Late";

function Dashboard() {
  const [range, setRange] = useState<RangePreset>("24h");
  const [tags, setTags] = useState<string[]>([]);
  // The previewed run: undefined until the user picks or closes one, so the
  // newest failure is shown on load; null once closed.
  const [picked, setPicked] = useState<number | null | undefined>(undefined);
  const preview = useRef<HTMLDivElement>(null);
  const { project, group } = useProject();
  const wide = useMinWidth(WIDE);
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
    queryKey: ["runs", "upcoming", project, UPCOMING_LIMIT],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/runs", {
          params: {
            query: {
              state_type: "Scheduled",
              sort: "scheduled_asc",
              scheduled_after: Date.now() * 1000,
              limit: UPCOMING_LIMIT,
              project,
            },
          },
        }),
      ),
    refetchInterval: 15_000,
  });
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const runs = recent.data?.items ?? [];
  const byType = counts.data?.runs ?? {};
  const inRange = (t: StateType) => runs.filter((r) => r.state.type === t).length;
  const late = runs.filter(isLate).length;
  // Completed, Failed, Waiting for input and Scheduled always show; the rest only when they happen.
  const stats: { key: string; label: string; state: StateType | "Late"; value: number; always?: boolean }[] =
    [
      { key: "Running", label: "Running", state: "Running", value: byType.Running ?? 0 },
      {
        key: "Completed",
        label: "Completed",
        state: "Completed",
        value: inRange("Completed"),
        always: true,
      },
      { key: "Failed", label: "Failed", state: "Failed", value: inRange("Failed"), always: true },
      { key: "Crashed", label: "Crashed", state: "Crashed", value: inRange("Crashed") },
      {
        key: "Paused",
        label: "Waiting for input",
        state: "Paused",
        value: byType.Paused ?? 0,
        always: true,
      },
      { key: "Late", label: "Late", state: "Late", value: late },
      {
        key: "Scheduled",
        label: "Scheduled",
        state: "Scheduled",
        value: byType.Scheduled ?? 0,
        always: true,
      },
    ];
  const rank = (r: Run) => (isLate(r) ? ATTENTION.length : ATTENTION.indexOf(r.state.type));
  const attention = runs
    .filter((r) => ATTENTION.includes(r.state.type) || isLate(r))
    .sort((a, b) => rank(a) - rank(b));
  const running = runs.filter((r) => r.state.type === "Running").slice(0, 6);
  // Derived from the same runs as the stat row, so it shares its bound of 500 runs in range.
  const completed = runs
    .filter((r) => r.state.type === "Completed")
    .sort((a, b) => finishedAt(b) - finishedAt(a))
    .slice(0, COMPLETED_LIMIT);
  const queue = useQuery({
    queryKey: ["queue"],
    queryFn: async () => unwrap(await api.GET("/api/queue")),
    // Only the empty Running now card reads it, for its idle processor count.
    enabled: recent.isSuccess && running.length === 0,
    refetchInterval: 10_000,
  });
  const allFlows = flows.data ?? [];
  const listed = dashboardFlows(allFlows, project, group);
  const scopedCount = allFlows.filter(
    (f) => (!project || f.project === project) && (!group || groupOf(f) === group),
  ).length;
  const selected = picked === undefined ? defaultPreviewRun(listed) : picked;
  const toggle = (id: number) => setPicked(selected === id ? null : id);
  const openFromTable = (id: number) => {
    setPicked(id);
    // The preview sits in the Flows card above; bring it into view.
    requestAnimationFrame(() => preview.current?.scrollIntoView?.({ block: "nearest", behavior: "smooth" }));
  };
  return (
    <Page
      title="Dashboard"
      // Cards clip their overflow, which would otherwise let the page column shrink them.
      className="[&>*]:shrink-0"
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
      <Card className="gap-4 px-5 py-5" data-testid="stat-card">
        <div className="flex flex-wrap items-end gap-x-9 gap-y-3">
          {stats
            .filter((s) => s.always || s.value > 0)
            .map((s) => (
              <div key={s.key} className="flex min-w-24 flex-col gap-0.5" data-testid={`count-${s.key}`}>
                <span className="text-[26px] font-semibold leading-8 tracking-tight tabular-nums">
                  {s.value}
                </span>
                <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                  {s.state === "Late" ? (
                    <span className="inline-block h-2.5 w-2.5 rounded-full bg-orange-500" />
                  ) : (
                    <StateDot type={s.state} />
                  )}
                  {s.label}
                </span>
              </div>
            ))}
          <div className="ml-auto flex flex-col items-end gap-0.5" data-testid="count-total">
            <span className="text-[26px] font-semibold leading-8 tracking-tight tabular-nums">
              {runs.length}
            </span>
            <span className="text-xs text-muted-foreground">runs in range</span>
          </div>
        </div>
        <Histogram
          runs={runs}
          start={start ?? runs.at(-1)?.created_at ?? now - 86_400_000_000}
          end={now}
          buckets={wide ? 48 : 24}
        />
      </Card>

      <FlowsCard
        flows={listed}
        total={scopedCount}
        selected={selected}
        onSelect={toggle}
        onClose={() => setPicked(null)}
        previewRef={preview}
      />

      <div
        className="grid grid-cols-2 items-stretch gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,2fr)_minmax(0,1fr)]"
        data-testid="dashboard-lists"
      >
        <UpcomingCard runs={upcoming.data?.items} total={byType.Scheduled ?? 0} />
        <AttentionCard runs={attention} />
        <RunningCard
          runs={running}
          count={byType.Running ?? running.length}
          idle={queue.data ? idleProcessors(queue.data.processors) : null}
          now={now}
        />
      </div>

      <CompletedCard runs={completed} onOpen={openFromTable} />
    </Page>
  );
}

/** A list card's header: 48 px tall so the three cards in a row line up. */
function Head({ id, title, aside }: { id: string; title: string; aside?: React.ReactNode }) {
  return (
    <div className="flex h-12 shrink-0 items-center justify-between gap-3 border-b px-4">
      <h2 id={id} className="font-semibold">
        {title}
      </h2>
      {aside}
    </div>
  );
}

function Pill({ children, tone }: { children: React.ReactNode; tone: "failure" | "muted" }) {
  return (
    <span
      className={cn(
        "inline-flex h-5.5 items-center rounded-full px-2 text-xs font-semibold tabular-nums",
        tone === "failure"
          ? "bg-red-100 text-red-700 dark:bg-red-900/40 dark:text-red-200"
          : "bg-muted text-muted-foreground",
      )}
    >
      {children}
    </span>
  );
}

/**
 * Up to eight flows that have run, failing first, each with a status line and
 * a strip of its last ten runs. A square opens that run's logs below the rows.
 */
function FlowsCard({
  flows,
  total,
  selected,
  onSelect,
  onClose,
  previewRef,
}: {
  flows: Flow[];
  total: number;
  selected: number | null;
  onSelect: (id: number) => void;
  onClose: () => void;
  previewRef: React.RefObject<HTMLDivElement | null>;
}) {
  return (
    <Card className="gap-0 overflow-hidden py-0" aria-labelledby="dash-flows" data-testid="flows-card">
      <div className="flex min-h-12 flex-wrap items-center justify-between gap-3 border-b px-4 py-2">
        <h2 id="dash-flows" className="font-semibold">
          Flows
        </h2>
        <div className="flex flex-wrap items-center justify-end gap-4">
          <RunStripLegend />
          <Link to="/flows" className="text-xs text-muted-foreground hover:underline">
            All {total} flows
          </Link>
        </div>
      </div>
      {flows.length === 0 ? (
        <div className="px-4 py-6 text-muted-foreground">No flow has run yet.</div>
      ) : (
        <div className="grid grid-cols-[repeat(auto-fill,minmax(340px,1fr))]">
          {flows.map((f) => (
            <FlowRow key={f.id} flow={f} selected={selected} onSelect={onSelect} />
          ))}
        </div>
      )}
      <div ref={previewRef}>
        {selected != null ? (
          <LogPreview key={selected} runId={selected} onClose={onClose} />
        ) : flows.length ? (
          <div className="border-t px-4 py-3 text-xs text-muted-foreground">
            Click a run square to see its logs here.
          </div>
        ) : null}
      </div>
    </Card>
  );
}

function FlowRow({
  flow,
  selected,
  onSelect,
}: {
  flow: Flow;
  selected: number | null;
  onSelect: (id: number) => void;
}) {
  const status = flowStatusLine(flow);
  return (
    <div
      className="-mb-px flex min-w-0 items-center gap-4 border-b px-4 py-3"
      data-testid="dashboard-flow"
      data-flow={flow.name}
    >
      <div className="min-w-0 flex-1">
        <Link
          to="/flows/$flowId"
          params={{ flowId: String(flow.id) }}
          className="block truncate font-medium hover:underline"
        >
          {flow.name}
        </Link>
        <div
          className={cn(
            "truncate text-xs",
            status.failing ? "text-red-700 dark:text-red-300" : "text-muted-foreground",
          )}
          data-testid="flow-status"
        >
          {status.text}
        </div>
      </div>
      <RunStrip
        runs={flow.recent_runs as RecentRun[]}
        selectedId={selected}
        onSelect={([id]) => onSelect(id)}
      />
    </div>
  );
}

/** The next three runs due, each with how soon, its flow, and why it is due. */
function UpcomingCard({ runs, total }: { runs: Run[] | undefined; total: number }) {
  return (
    <Card className="gap-0 overflow-hidden py-0" aria-labelledby="dash-upcoming" data-testid="upcoming-card">
      <Head
        id="dash-upcoming"
        title="Upcoming"
        aside={
          <Link to="/queue" className="text-xs text-muted-foreground hover:underline">
            All {total}
          </Link>
        }
      />
      {runs && runs.length === 0 ? <Empty>Nothing scheduled.</Empty> : null}
      <ol className="flex-1 py-1.5">
        {(runs ?? []).slice(0, UPCOMING_LIMIT).map((r) => {
          const at = r.scheduled_time ?? r.created_at;
          return (
            <li
              key={r.id}
              className="grid grid-cols-[88px_minmax(0,1fr)] items-baseline gap-3 px-4 py-2"
              data-run-id={r.id}
              data-testid="upcoming-row"
            >
              <span
                className="whitespace-nowrap font-mono text-xs text-amber-800 dark:text-amber-300"
                title={formatClock(at)}
              >
                {formatIn(at)}
              </span>
              <span className="min-w-0">
                <Link
                  to="/flows/$flowId"
                  params={{ flowId: String(r.flow_id) }}
                  className="block truncate font-medium hover:underline"
                >
                  {r.flow_name}
                </Link>
                <span className="block text-xs text-muted-foreground">{upcomingReason(r)}</span>
              </span>
            </li>
          );
        })}
      </ol>
    </Card>
  );
}

/** Paused, Failed, Crashed and Late runs: five at most, then a link to the rest. */
function AttentionCard({ runs }: { runs: Run[] }) {
  const types = Array.from(new Set(runs.map((r) => r.state.type)));
  return (
    <Card
      className="order-first col-span-2 gap-0 overflow-hidden py-0 xl:order-none xl:col-span-1"
      aria-labelledby="dash-attention"
      data-testid="attention-card"
    >
      <Head
        id="dash-attention"
        title="Needs attention"
        aside={runs.length ? <Pill tone="failure">{runs.length}</Pill> : null}
      />
      {runs.length === 0 ? <Empty>Nothing waiting on you.</Empty> : null}
      {runs.slice(0, ATTENTION_LIMIT).map((r) => (
        <AttentionRow key={r.id} run={r} />
      ))}
      {runs.length > ATTENTION_LIMIT ? (
        <Link
          to="/runs"
          // The Runs page filters on one state; with several, it lists them all.
          search={{ tab: "runs", state: types.length === 1 ? types[0] : undefined }}
          className="px-4 py-3 text-xs text-muted-foreground hover:underline"
        >
          Show all {runs.length}
        </Link>
      ) : null}
    </Card>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return <div className="px-4 py-6 text-muted-foreground">{children}</div>;
}

/** The state of an attention run in words, after its name. */
function stateWords(run: Run): string {
  if (isLate(run)) return "is late";
  if (run.state.type === "Paused") return "is waiting for input";
  return run.state.name.toLowerCase();
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
      navigate({
        to: "/runs/$runId",
        params: { runId: String("conflict" in created ? created.run.id : created.id) },
      });
    },
  });
  const details = (run.state.details ?? {}) as { prompt?: string };
  const message =
    run.state.type === "Paused"
      ? (details.prompt ?? run.state.message ?? "Waiting for input")
      : isLate(run)
        ? `Scheduled for ${formatClock(run.scheduled_time ?? run.created_at)}`
        : (run.state.message ?? "");
  const open = () => navigate({ to: "/runs/$runId", params: { runId: String(run.id) } });
  const failed = run.state.type === "Failed" || run.state.type === "Crashed";
  return (
    <div
      className="flex items-center gap-3.5 border-b px-4 py-3 last:border-b-0"
      data-run-id={run.id}
      data-testid="attention-row"
    >
      {isLate(run) ? (
        <span className="inline-block h-2.5 w-2.5 shrink-0 rounded-full bg-orange-500" />
      ) : (
        <StateDot type={run.state.type} title={run.state.name} />
      )}
      <div className="min-w-0 flex-1">
        <div className="truncate">
          <Link
            to="/runs/$runId"
            params={{ runId: String(run.id) }}
            className="font-semibold hover:underline"
          >
            {run.name}
          </Link>{" "}
          <span className="text-muted-foreground">{stateWords(run)}</span>
        </div>
        <div className={cn("truncate text-xs text-muted-foreground", failed && "font-mono")} title={message}>
          {message || `${run.project}/${run.flow_name}`}
        </div>
      </div>
      {run.state.type === "Paused" ? (
        <Button size="sm" onClick={open} data-testid="answer-run">
          Answer
        </Button>
      ) : failed ? (
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

function RunningCard({
  runs,
  count,
  idle,
  now,
}: {
  runs: Run[];
  count: number;
  idle: number | null;
  now: number;
}) {
  return (
    <Card className="gap-0 overflow-hidden py-0" aria-labelledby="dash-running" data-testid="running-card">
      <Head id="dash-running" title="Running now" aside={<Pill tone="muted">{count}</Pill>} />
      {runs.length === 0 ? (
        <div
          className="flex flex-1 flex-col items-center justify-center gap-2.5 px-4 py-6 text-center"
          data-testid="running-empty"
        >
          <span className="inline-flex size-9 items-center justify-center rounded-full bg-muted text-muted-foreground">
            <Pause className="size-4" />
          </span>
          <div className="font-medium">Nothing running</div>
          {idle != null ? (
            <div className="text-xs text-muted-foreground">
              {idle} {idle === 1 ? "processor" : "processors"} idle
            </div>
          ) : null}
        </div>
      ) : null}
      {runs.map((r) => (
        <div key={r.id} className="flex flex-col gap-2 border-b px-4 py-3 last:border-b-0" data-run-id={r.id}>
          <div className="flex min-w-0 items-baseline gap-2">
            <StateDot type="Running" />
            <Link
              to="/runs/$runId"
              params={{ runId: String(r.id) }}
              className="whitespace-nowrap font-medium hover:underline"
            >
              {r.name}
            </Link>
            <span className="min-w-0 truncate text-xs text-muted-foreground">{r.flow_name}</span>
            <span className="ml-auto whitespace-nowrap font-mono text-xs tabular-nums text-muted-foreground">
              {elapsed(r, now)}
            </span>
          </div>
          <TaskProgress counts={r.task_counts ?? {}} />
        </div>
      ))}
    </Card>
  );
}

/** The latest Completed runs in the range; a name opens its logs in the Flows card. */
function CompletedCard({ runs, onOpen }: { runs: Run[]; onOpen: (id: number) => void }) {
  return (
    <Card
      className="gap-0 overflow-hidden py-0"
      aria-labelledby="dash-completed"
      data-testid="recently-completed"
    >
      <Head
        id="dash-completed"
        title="Recently completed"
        aside={
          <Link to="/runs" search={{ tab: "runs" }} className="text-xs text-muted-foreground hover:underline">
            All runs
          </Link>
        }
      />
      <Table>
        <thead>
          <tr>
            <Th>Run</Th>
            <Th>Flow</Th>
            <Th>Finished</Th>
            <Th className="text-right">Duration</Th>
          </tr>
        </thead>
        <tbody>
          {runs.map((r) => (
            <Tr key={r.id} data-run-id={r.id}>
              <Td>
                <span className="inline-flex items-center gap-2.5">
                  <StateDot type={r.state.type} title={r.state.name} />
                  <button
                    type="button"
                    onClick={() => onOpen(r.id)}
                    className="cursor-pointer font-medium underline decoration-border underline-offset-[3px] hover:decoration-foreground"
                    data-testid="completed-name"
                  >
                    {r.name}
                  </button>
                </span>
              </Td>
              <Td className="text-muted-foreground">{r.flow_name}</Td>
              <Td className="whitespace-nowrap text-muted-foreground" title={formatClock(finishedAt(r))}>
                {relativeTime(finishedAt(r))}
              </Td>
              <Td className="text-right font-mono text-xs tabular-nums">
                {formatDuration(r.total_run_time)}
              </Td>
            </Tr>
          ))}
          {runs.length === 0 ? (
            <tr>
              <td colSpan={4} className="px-4 py-6 text-center text-muted-foreground">
                No completed runs in this range.
              </td>
            </tr>
          ) : null}
        </tbody>
      </Table>
    </Card>
  );
}
