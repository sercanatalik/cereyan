import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { Minus, Pause, Play, Plus } from "lucide-react";
import { useState } from "react";
import { ApiError, api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { CardHead } from "@/components/ui/card";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { useLiveEvent } from "@/lib/live";
import { cn, formatDuration, formatIn } from "@/lib/utils";

type QueueView = components["schemas"]["QueueView"];
type EngineView = components["schemas"]["EngineView"];
type InLine = components["schemas"]["InLine"];

export const Route = createFileRoute("/queue")({ component: QueuePage });

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

function ProcessorTile({ n, engine }: { n: number; engine?: EngineView }) {
  if (!engine) {
    return (
      <div
        className="flex min-h-28 flex-col justify-between rounded-lg border border-dashed p-3 text-muted-foreground"
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
      className={cn("flex min-h-28 flex-col justify-between gap-2 rounded-lg border p-3", s.tile)}
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
        <div className="flex flex-col">
          <Link
            to="/runs/$runId"
            params={{ runId: String(engine.run_id) }}
            className="font-medium hover:underline"
          >
            Run #{engine.run_id}
          </Link>
          <span className="font-mono text-xs text-muted-foreground">
            {formatDuration(engine.since_secs * 1_000_000)}
          </span>
        </div>
      ) : (
        <span className="text-muted-foreground">
          {engine.status === "starting" ? "Starting…" : "Warm, waiting for work"}
        </span>
      )}
      <span className="truncate border-t border-dashed pt-2 font-mono text-xs text-muted-foreground">
        {engine.module}
      </span>
    </div>
  );
}

function ProcessorsCard({ view }: { view: QueueView }) {
  const client = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: async () => unwrap(await api.GET("/api/settings", {})),
  });
  const { count, cap, items, load } = view.processors;
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
  const idle = items.length - busy - draining;
  // Live engines fill the first slots; draining ones follow, past the count.
  const live = items.filter((e) => e.status !== "draining");
  const tiles: { n: number; engine?: EngineView }[] = [];
  for (let i = 0; i < Math.max(count, live.length); i++) tiles.push({ n: i + 1, engine: live[i] });
  for (const engine of items.filter((e) => e.status === "draining")) {
    tiles.push({ n: tiles.length + 1, engine });
  }
  const loadPct = load == null ? null : Math.round(Math.min(1, load) * 100);
  return (
    <section
      className="rounded-lg border bg-card"
      aria-labelledby="processors-h"
      data-testid="processors-card"
    >
      <div className="flex flex-wrap items-center justify-between gap-4 border-b px-4 py-3">
        <div className="flex items-baseline gap-3">
          <h2 id="processors-h" className="font-semibold">
            Processors
          </h2>
          <span className="text-muted-foreground">
            {busy} busy · {idle} idle{draining ? ` · ${draining} draining` : ""}
          </span>
        </div>
        <div className="flex items-center gap-5">
          {loadPct != null ? (
            <div
              className="flex items-center gap-2 text-muted-foreground"
              title="One-minute load average per CPU"
            >
              <span>CPU</span>
              <div className="h-1.5 w-28 overflow-hidden rounded-full bg-muted">
                <div
                  className={cn("h-1.5", loadPct > 85 ? "bg-amber-600" : "bg-foreground")}
                  style={{ width: `${loadPct}%` }}
                />
              </div>
              <span className="font-mono text-foreground">{loadPct}%</span>
            </div>
          ) : null}
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
      <div className="grid grid-cols-[repeat(auto-fill,minmax(200px,1fr))] gap-3 p-4">
        {tiles.map((t) => (
          <ProcessorTile key={t.engine?.id ?? `slot-${t.n}`} n={t.n} engine={t.engine} />
        ))}
      </div>
      <p className="px-4 pb-3 text-xs text-muted-foreground">
        A processor is an engine process: it loads a flow's module when it takes that flow's run. The count is
        saved in Settings and can't go above this machine's CPU count. Removing a processor never interrupts a
        run: it finishes its current run, then exits. Processors bound how many runs execute at once; tasks
        inside a run still run concurrently.
      </p>
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

function QueuePage() {
  const client = useQueryClient();
  const queue = useQuery({
    queryKey: ["queue"],
    queryFn: async () => unwrap(await api.GET("/api/queue")),
    refetchInterval: 5000,
  });
  useLiveEvent("run.updated", () => client.invalidateQueries({ queryKey: ["queue"] }));
  const view = queue.data;
  return (
    <Page
      title="Queue"
      subtitle="Every run waits in one line. A free processor takes the first run that can start: priority first, then time in line."
    >
      {view ? (
        <>
          <ProcessorsCard view={view} />
          <div className="grid grid-cols-1 gap-5 xl:grid-cols-3">
            <section className="rounded-lg border bg-card xl:col-span-2" aria-label="In line">
              <CardHead title="In line" aside={`${view.in_line.length + view.more} runs`} />
              {view.in_line.length === 0 ? (
                <p className="px-4 py-6 text-muted-foreground">Nothing is waiting. New runs start at once.</p>
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
            <section className="self-start rounded-lg border bg-card" aria-label="Joining the line">
              <CardHead title="Joining the line" aside="holds no processor while waiting" />
              {view.joining.length === 0 && view.paused_loops.length === 0 ? (
                <p className="px-4 py-6 text-muted-foreground">Nothing joins the line in the next hour.</p>
              ) : (
                <ul>
                  {view.joining.map((j) => (
                    <li
                      key={j.run_id}
                      className="flex items-center justify-between gap-3 border-t px-4 py-2.5 first:border-t-0"
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
                      className="flex items-center justify-between gap-3 border-t px-4 py-2.5 first:border-t-0"
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
          </div>
        </>
      ) : queue.isError ? (
        <p className="text-destructive">Could not load the queue.</p>
      ) : (
        <p className="text-muted-foreground">Loading…</p>
      )}
    </Page>
  );
}
