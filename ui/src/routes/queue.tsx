import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { Minus, Pause, Play, Plus } from "lucide-react";
import { useState } from "react";
import { ApiError, api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { QueueSparkline } from "@/components/queue-sparkline";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { UnderlineTabs } from "@/components/ui/underline-tabs";
import { WorkersTab } from "@/components/workers-tab";
import { useLiveEvent } from "@/lib/live";
import { queueSummary } from "@/lib/summary/queue";
import { cn, formatDuration, formatIn } from "@/lib/utils";

type QueueView = components["schemas"]["QueueView"];
type EngineView = components["schemas"]["EngineView"];
type InLine = components["schemas"]["InLine"];

export const Route = createFileRoute("/queue")({
  validateSearch: (s: Record<string, unknown>): { tab?: "workers" } => ({
    tab: s.tab === "workers" ? "workers" : undefined,
  }),
  component: QueuePage,
});

const STATUS: Record<string, { label: string; pill: string; dot: string; tile: string }> = {
  running: {
    label: "Running",
    pill: "bg-sky-100 text-sky-900 dark:bg-sky-900/40 dark:text-sky-200",
    dot: "bg-sky-500",
    tile: "border-border bg-card",
  },
  draining: {
    label: "Draining",
    pill: "bg-amber-100 text-amber-900 dark:bg-amber-900/40 dark:text-amber-200",
    dot: "bg-amber-500",
    tile: "border-amber-300 bg-amber-50 dark:border-amber-800 dark:bg-amber-950/30",
  },
  idle: {
    label: "Idle",
    pill: "bg-muted text-muted-foreground",
    dot: "bg-zinc-400",
    tile: "border-dashed bg-background",
  },
  starting: {
    label: "Starting",
    pill: "bg-muted text-muted-foreground",
    dot: "bg-zinc-300",
    tile: "border-dashed bg-background",
  },
};

/** Why a run in line can or cannot start, as the table's chip reads it. */
export function reasonText(row: Pick<InLine, "can_start" | "reason" | "position">): string {
  if (row.can_start) return row.position === 1 ? "Next" : "Ready";
  const reason = row.reason ?? "";
  if (reason.startsWith("resource:")) return `Waiting for ${reason.slice("resource:".length)}`;
  if (reason === "max_concurrent") return "At max_concurrent";
  if (reason === "backfill concurrency") return "At backfill concurrency";
  if (reason === "no processor") return "No free processor";
  if (reason === "no processor with matching code") return "No processor with matching code";
  return reason;
}

function Pill({ className, children }: { className: string; children: React.ReactNode }) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium",
        className,
      )}
    >
      {children}
    </span>
  );
}

/** The run a busy processor is executing: its name, flow, and the task running now. */
function SlotRun({ runId, since }: { runId: number; since: number }) {
  // The run detail page's keys, so opening the run from here is instant.
  const run = useQuery({
    queryKey: ["run", runId],
    queryFn: async () => unwrap(await api.GET("/api/runs/{id}", { params: { path: { id: runId } } })),
  });
  const tasks = useQuery({
    queryKey: ["run-tasks", runId],
    queryFn: async () => unwrap(await api.GET("/api/runs/{id}/tasks", { params: { path: { id: runId } } })),
    refetchInterval: 5000,
  });
  const running = (tasks.data ?? []).filter((t) => t.state.type === "Running").map((t) => t.dynamic_key);
  const r = run.data;
  return (
    <div className="flex min-w-0 flex-col">
      <div className="flex items-baseline justify-between gap-2">
        <Link
          to="/runs/$runId"
          params={{ runId: String(runId) }}
          className="truncate font-medium hover:underline"
        >
          {r ? r.name : `Run #${runId}`}
        </Link>
        <span className="font-mono text-xs text-muted-foreground">{formatDuration(since * 1_000_000)}</span>
      </div>
      {r ? (
        <span className="truncate text-xs text-muted-foreground" data-testid="slot-detail">
          {r.project}/{r.flow_name}
          {running.length
            ? ` · task ${running[0]}${running.length > 1 ? ` + ${running.length - 1} more` : ""}`
            : ""}
        </span>
      ) : null}
    </div>
  );
}

function ProcessorTile({ n, engine }: { n: number; engine?: EngineView }) {
  if (!engine) {
    return (
      <div
        className="flex min-h-24 flex-col justify-between gap-2 rounded-lg border border-dashed p-3 text-muted-foreground"
        data-testid="processor-tile"
        data-status="available"
      >
        <span className="font-mono text-xs">P{n}</span>
        <span>Available: starts when a run needs it</span>
      </div>
    );
  }
  const s = STATUS[engine.status] ?? STATUS.idle;
  return (
    <div
      className={cn("flex min-h-24 flex-col justify-between gap-2 rounded-lg border p-3", s.tile)}
      data-testid="processor-tile"
      data-status={engine.status}
    >
      <div className="flex items-center justify-between">
        <span className="font-mono text-xs text-muted-foreground">P{n}</span>
        <Pill className={s.pill}>
          <span className={cn("size-1.5 rounded-full", s.dot)} />
          {s.label}
        </Pill>
      </div>
      {engine.run_id ? (
        <SlotRun runId={engine.run_id} since={engine.since_secs} />
      ) : (
        <span className="flex min-w-0 flex-col">
          <span className="text-muted-foreground">
            {engine.status === "starting" ? "Starting…" : "Ready for a run"}
          </span>
          <span className="truncate font-mono text-xs text-muted-foreground" title={engine.module}>
            Loaded: {engine.module}
          </span>
        </span>
      )}
    </div>
  );
}

/** Busy against total, CPU, queue depth, the processor stepper, and a slot per processor. */
function CapacityCard({ view }: { view: QueueView }) {
  const client = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: async () => unwrap(await api.GET("/api/settings", {})),
  });
  const { count, cap, load } = view.processors;
  const all = view.processors.items;
  // The server's own processors; each worker's are listed below them.
  const items = all.filter((e) => (e.host ?? "server") === "server");
  const remote = (view.processors.hosts ?? []).filter((h) => h.host !== "server");
  const resize = useMutation({
    mutationFn: async (n: number) => unwrap(await api.PATCH("/api/settings", { body: { max_engines: n } })),
    onSuccess: () => {
      setError(null);
      client.invalidateQueries({ queryKey: ["queue"] });
      client.invalidateQueries({ queryKey: ["settings"] });
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
  });
  const busy = items.filter((e) => e.status === "running").length;
  const draining = items.filter((e) => e.status === "draining").length;
  // Live engines fill the first slots; draining ones follow, past the count.
  const live = items.filter((e) => e.status !== "draining");
  const tiles: { n: number; engine?: EngineView }[] = [];
  for (let i = 0; i < Math.max(count, live.length); i++) tiles.push({ n: i + 1, engine: live[i] });
  for (const engine of items.filter((e) => e.status === "draining")) {
    tiles.push({ n: tiles.length + 1, engine });
  }
  const loadPct = load == null ? null : Math.round(Math.min(1, load) * 100);
  return (
    <section className="rounded-lg border bg-card" aria-labelledby="capacity-h" data-testid="processors-card">
      <div className="flex flex-wrap items-start justify-between gap-4 border-b px-4 py-3">
        <div className="flex flex-col gap-1">
          <h2 id="capacity-h" className="font-semibold">
            Capacity
          </h2>
          <span className="text-muted-foreground" data-testid="capacity-busy">
            {busy} of {count} {count === 1 ? "processor" : "processors"} busy
            {draining ? ` · ${draining} draining` : ""}
            {loadPct != null ? ` · machine CPU at ${loadPct}%` : ""}
          </span>
          {loadPct != null ? (
            <div
              className="h-1.5 w-40 overflow-hidden rounded-full bg-muted"
              title="One-minute load average per CPU"
            >
              <div
                className={cn("h-1.5", loadPct > 85 ? "bg-amber-600" : "bg-foreground")}
                style={{ width: `${loadPct}%` }}
              />
            </div>
          ) : null}
        </div>
        <QueueSparkline width={180} height={28} />
      </div>
      <div className="flex flex-wrap items-center gap-3 px-4 pt-3">
        <span className="font-medium">Processors</span>
        <div className="flex items-center overflow-hidden rounded-md border">
          <Button
            variant="ghost"
            size="icon"
            className="rounded-none"
            aria-label="Remove a processor"
            disabled={count <= 1 || resize.isPending}
            onClick={() => resize.mutate(count - 1)}
          >
            <Minus className="size-4" />
          </Button>
          <div className="min-w-24 border-x px-3 text-center leading-9" data-testid="processor-count">
            <span className="font-mono text-base font-medium">{count}</span>
            <span className="text-muted-foreground"> / {cap} CPUs</span>
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="rounded-none"
            aria-label={count >= cap ? `Add a processor (at the ${cap} CPU cap)` : "Add a processor"}
            disabled={count >= cap || resize.isPending}
            onClick={() => resize.mutate(count + 1)}
          >
            <Plus className="size-4" />
          </Button>
        </div>
      </div>
      {settings.data?.engine_saturation_risk ? (
        <div
          className="mx-4 mt-3 flex items-center justify-between gap-3 rounded-md border border-amber-400 bg-amber-50 px-3 py-2 text-amber-900 dark:bg-amber-900/30 dark:text-amber-100"
          data-testid="saturation-banner"
        >
          <span>These flows can occupy every processor: {settings.data.saturation_flows.join(", ")}.</span>
          {count < cap ? (
            <Button size="sm" variant="outline" onClick={() => resize.mutate(count + 1)}>
              Add a processor
            </Button>
          ) : null}
        </div>
      ) : null}
      {error ? <div className="mx-4 mt-3 text-destructive">{error}</div> : null}
      <div className="grid grid-cols-[repeat(auto-fill,minmax(220px,1fr))] gap-3 p-4">
        {tiles.map((t) => (
          <ProcessorTile key={t.engine?.id ?? `slot-${t.n}`} n={t.n} engine={t.engine} />
        ))}
      </div>
      {remote.map((h) => {
        const mine = all.filter((e) => e.host === h.host);
        const slots: { n: number; engine?: EngineView }[] = [];
        for (let i = 0; i < Math.max(h.count, mine.length); i++) slots.push({ n: i + 1, engine: mine[i] });
        return (
          <div key={h.host} className="border-t px-4 pt-3 pb-4" data-testid="host-group">
            <div className="mb-2 flex items-center gap-2 text-xs text-muted-foreground">
              <span className="font-semibold text-foreground">{h.host}</span>
              <span>
                {h.state} · {h.busy} busy / {h.count}
              </span>
              <Link to="/queue" search={{ tab: "workers" }} className="underline">
                details
              </Link>
            </div>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(220px,1fr))] gap-3">
              {slots.map((t) => (
                <ProcessorTile key={t.engine?.id ?? `${h.host}-${t.n}`} n={t.n} engine={t.engine} />
              ))}
            </div>
          </div>
        );
      })}
    </section>
  );
}

/** Pause or resume a continuous schedule's loop from its row. */
function LoopToggle({ sid, flow, paused }: { sid: number; flow: string; paused: boolean }) {
  const client = useQueryClient();
  const toggle = useMutation({
    mutationFn: async () =>
      unwrap(
        paused
          ? await api.POST("/api/schedules/{sid}/resume", { params: { path: { sid } } })
          : await api.POST("/api/schedules/{sid}/pause", { params: { path: { sid } } }),
      ),
    onSuccess: () => client.invalidateQueries({ queryKey: ["queue"] }),
  });
  return (
    <Button
      variant="outline"
      size="icon-sm"
      aria-label={`${paused ? "Resume" : "Pause"} the ${flow} loop`}
      disabled={toggle.isPending}
      onClick={() => toggle.mutate()}
    >
      {paused ? <Play className="size-3.5" /> : <Pause className="size-3.5" />}
    </Button>
  );
}

function reasonClass(row: InLine): string {
  if (row.can_start && row.position === 1)
    return "bg-sky-100 text-sky-900 dark:bg-sky-900/40 dark:text-sky-200";
  if (row.can_start) return "bg-muted text-foreground";
  return "bg-amber-100 text-amber-900 dark:bg-amber-900/40 dark:text-amber-200";
}

/** A titled part of the Up next card, its count beside the title. */
function SectionHead({ title, count, note }: { title: string; count: number; note?: string }) {
  return (
    <h3 className="flex items-baseline gap-2 px-4 pt-3 pb-2 text-sm">
      <span className="font-semibold">{title}</span>
      <span className="font-mono text-muted-foreground">{count}</span>
      {note ? <span className="text-xs font-normal text-muted-foreground">· {note}</span> : null}
    </h3>
  );
}

/** What starts next: runs in line that can take a processor, then runs not yet due. */
function UpNextCard({ view }: { view: QueueView }) {
  const later = view.joining.length + view.paused_loops.length;
  return (
    <section className="rounded-lg border bg-card" aria-labelledby="up-next-h">
      <div className="flex items-center justify-between border-b px-4 py-3">
        <h2 id="up-next-h" className="font-semibold">
          Up next
        </h2>
        <span className="text-xs text-muted-foreground">Priority first, then time in line</span>
      </div>
      <section aria-label="Ready to start">
        <SectionHead title="Ready to start" count={view.in_line.length + view.more} />
        {view.in_line.length === 0 ? (
          <p className="px-4 pb-4 text-muted-foreground">No run is waiting for a processor.</p>
        ) : (
          <Table>
            <thead>
              <tr>
                <Th className="w-10">#</Th>
                <Th>Run</Th>
                <Th>Trigger</Th>
                <Th>Can start?</Th>
                <Th className="text-right">Priority</Th>
                <Th className="text-right">In line</Th>
              </tr>
            </thead>
            <tbody>
              {view.in_line.map((r) => (
                <Tr key={r.run_id} data-testid="in-line-row">
                  <Td className="font-mono text-muted-foreground">{r.position}</Td>
                  <Td>
                    <div className="flex flex-col">
                      <Link
                        to="/runs/$runId"
                        params={{ runId: String(r.run_id) }}
                        className="font-medium hover:underline"
                      >
                        {r.flow}
                      </Link>
                      <span className="font-mono text-xs text-muted-foreground">
                        #{r.run_id} · {r.module}
                      </span>
                    </div>
                  </Td>
                  <Td className="text-muted-foreground">{r.trigger}</Td>
                  <Td>
                    <Pill className={reasonClass(r)}>{reasonText(r)}</Pill>
                    {r.overtaken_by > 0 ? (
                      <div className="text-xs text-muted-foreground">
                        {r.overtaken_by} {r.overtaken_by === 1 ? "run" : "runs"} went ahead
                      </div>
                    ) : null}
                  </Td>
                  <Td className="text-right font-mono">{r.priority}</Td>
                  <Td className="text-right font-mono text-muted-foreground">
                    {formatDuration(r.waited_us)}
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        )}
        {view.more > 0 ? (
          <p className="border-t px-4 py-2 text-xs text-muted-foreground">and {view.more} more</p>
        ) : null}
      </section>
      <section className="border-t" aria-label="Starting later">
        <SectionHead title="Starting later" count={later} note="these hold no processor while they wait" />
        {later === 0 ? (
          <p className="px-4 pb-4 text-muted-foreground">Nothing joins the line in the next hour.</p>
        ) : (
          <ul>
            {view.joining.map((j) => (
              <li
                key={j.run_id}
                className="flex items-center justify-between gap-3 border-t px-4 py-2.5"
                data-testid="joining-row"
              >
                <div className="flex min-w-0 flex-col">
                  <Link
                    to="/runs/$runId"
                    params={{ runId: String(j.run_id) }}
                    className="truncate font-medium hover:underline"
                  >
                    {j.flow}
                  </Link>
                  <span className="text-xs text-muted-foreground">{j.kind}</span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="font-mono whitespace-nowrap">{formatIn(j.at)}</span>
                  {j.kind === "continuous" && j.schedule_id != null ? (
                    <LoopToggle sid={j.schedule_id} flow={j.flow} paused={false} />
                  ) : null}
                </div>
              </li>
            ))}
            {view.paused_loops.map((l) => (
              <li
                key={`loop-${l.schedule_id}`}
                className="flex items-center justify-between gap-3 border-t px-4 py-2.5"
                data-testid="paused-loop-row"
              >
                <div className="flex min-w-0 flex-col">
                  <span className="truncate font-medium">{l.flow}</span>
                  <span className="text-xs text-muted-foreground">
                    {l.reason === "disabled"
                      ? `continuous · paused after failures${l.until ? `, resumes ${formatIn(l.until)}` : ""}`
                      : "continuous · paused"}
                  </span>
                </div>
                <div className="flex items-center gap-2">
                  <span className="font-mono text-muted-foreground">paused</span>
                  <LoopToggle sid={l.schedule_id} flow={l.flow} paused />
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>
    </section>
  );
}

/** The line and processors explained, closed until asked for. */
function QueueExplainer() {
  return (
    <details className="group rounded-lg border bg-card px-4 py-3" data-testid="queue-explainer">
      <summary className="cursor-pointer font-medium marker:text-muted-foreground">
        How the queue works
      </summary>
      <div className="mt-2 flex max-w-3xl flex-col gap-2 text-muted-foreground">
        <p>
          Every run waits in one line. A free processor takes the first run that can start: priority first,
          then time in line. A run that can't start yet, waiting for a resource or a concurrency limit, lets
          later runs go ahead of it.
        </p>
        <p>
          A processor is an engine process: it loads a flow's module when it takes that flow's run. The count
          is saved in Settings and can't go above this machine's CPU count. Removing a processor never
          interrupts a run: it finishes its current run, then exits. Processors bound how many runs execute at
          once; tasks inside a run still run concurrently.
        </p>
      </div>
    </details>
  );
}

function QueuePage() {
  const client = useQueryClient();
  const queue = useQuery({
    queryKey: ["queue"],
    queryFn: async () => unwrap(await api.GET("/api/queue")),
    refetchInterval: 5000,
  });
  useLiveEvent("run.updated", () => client.invalidateQueries({ queryKey: ["queue"] }));
  const view = queue.data;
  const { tab } = Route.useSearch();
  const navigate = Route.useNavigate();
  // Every host's processors count: a run executing on a worker is executing.
  const executing = (view?.processors.items ?? []).filter((e) => e.run_id != null).length;
  return (
    <Page
      title="Queue"
      subtitle={
        view ? (
          <span data-testid="queue-summary">
            {queueSummary({ executing, waiting: view.in_line.length + view.more })}
          </span>
        ) : null
      }
    >
      <UnderlineTabs
        items={[
          { value: "line", label: "Line" },
          { value: "workers", label: "Workers" },
        ]}
        value={tab ?? "line"}
        onChange={(v) => navigate({ search: v === "workers" ? { tab: "workers" } : {} })}
      />
      {tab === "workers" ? (
        <WorkersTab queue={view} />
      ) : view ? (
        <>
          <CapacityCard view={view} />
          <UpNextCard view={view} />
          <QueueExplainer />
        </>
      ) : queue.isError ? (
        <p className="text-destructive">Could not load the queue.</p>
      ) : (
        <p className="text-muted-foreground">Loading…</p>
      )}
    </Page>
  );
}
