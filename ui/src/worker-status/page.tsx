import { ArrowUpRight, Moon, Sun } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Mark, useTheme } from "@/components/mark";
import { Button } from "@/components/ui/button";
import { CardHead } from "@/components/ui/card";
import { HostCard, Pill, STATE_COLOUR, type WorkerStatus, workerStatus } from "@/components/worker-status";
import { workerHeadline } from "@/lib/summary/worker";
import { cn, formatClock, formatDuration, relativeTime } from "@/lib/utils";
import type { EngineStats, FlowStats, WorkerStatusPayload } from "./types";

/** Seconds between polls of `status.json`. */
const POLL_MS = 5000;

/**
 * Poll `status.json` (relative, so the page works under any proxy prefix),
 * keeping the last good payload when a fetch fails.
 */
export function useWorkerStatus(url = "status.json") {
  const [data, setData] = useState<WorkerStatusPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    const load = async () => {
      try {
        const r = await fetch(url, { cache: "no-store" });
        if (!r.ok) throw new Error(`status.json answered ${r.status}`);
        const body = (await r.json()) as WorkerStatusPayload;
        if (live) {
          setData(body);
          setError(null);
        }
      } catch (e) {
        if (live) setError(e instanceof Error ? e.message : String(e));
      }
    };
    load();
    const timer = setInterval(load, POLL_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [url]);
  return { data, error };
}

/**
 * The worker's status as its own page reads it: not reaching the server comes
 * first, then the states only the worker has, before it is registered.
 */
export function pageStatus(s: WorkerStatusPayload): WorkerStatus {
  if (!s.server_reachable) return "unreachable";
  if (s.state === "registering" || s.state === "refused") return s.state;
  return workerStatus({ state: s.state, drift: s.drift });
}

const uptime = (micros: number): string => {
  const minutes = Math.floor(micros / 60_000_000);
  if (minutes < 60) return `${minutes} m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours} h ${minutes % 60} m`;
  return `${Math.floor(hours / 24)} d ${hours % 24} h`;
};

function Banner({ tone, children }: { tone: "warn" | "bad"; children: React.ReactNode }) {
  return (
    <div
      className={cn(
        "rounded-md border px-3 py-2",
        tone === "bad"
          ? "border-red-200 bg-red-50 text-red-900 dark:border-red-900 dark:bg-red-950/40 dark:text-red-200"
          : "border-amber-300 bg-amber-50 text-amber-900 dark:border-amber-800 dark:bg-amber-950/30 dark:text-amber-200",
      )}
      data-testid="worker-banner"
    >
      {children}
    </div>
  );
}

function Fact({
  label,
  children,
  className,
}: {
  label: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div
      className="flex min-w-0 flex-col gap-0.5"
      data-testid={`fact-${label.toLowerCase().replace(/\s+/g, "-")}`}
    >
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className={cn("truncate text-sm font-medium", className)}>{children}</span>
    </div>
  );
}

/** A tile's number in its state's colour; the dot uses the shared run state colour. */
const TILE_TEXT: Record<string, string> = {
  Completed: "text-emerald-700 dark:text-emerald-400",
  Failed: "text-red-600 dark:text-red-400",
  Running: "text-sky-700 dark:text-sky-400",
};

/**
 * A count of runs in one state. Above zero, its dot and number take the
 * state's colour; the tile itself never fills. At zero, or unknown, it is muted.
 */
function Tile({
  state,
  count,
  note,
  stale,
  testId,
}: {
  state: "Completed" | "Failed" | "Running";
  count: number | null;
  note: React.ReactNode;
  stale?: boolean;
  testId?: string;
}) {
  const lit = count !== null && count > 0;
  return (
    <div
      className="flex min-w-0 flex-col gap-0.5 rounded-lg border bg-card px-4 py-3"
      data-testid={testId}
      data-state={state}
      data-muted={lit ? undefined : "true"}
    >
      <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <span
          className={cn("size-2 rounded-full", lit ? STATE_COLOUR[state] : "bg-border")}
          data-testid="tile-dot"
        />
        {state}
      </span>
      <span
        className={cn(
          "font-mono text-2xl font-medium",
          lit ? TILE_TEXT[state] : "text-muted-foreground",
          stale && "opacity-45",
        )}
        data-testid="tile-count"
      >
        {count ?? "-"}
      </span>
      <span className="text-xs text-muted-foreground">{note}</span>
    </div>
  );
}

/** Why a flow takes no run here: its code differs, or it only runs on the server. */
type FlowRow = {
  name: string;
  module: string | null;
  counts: FlowStats | null;
  blocked: "older code · takes no run" | "runs on the server only" | null;
};

const runsOf = (c: FlowStats | null) =>
  c ? c.completed + c.failed + c.crashed + c.cancelled + c.running : 0;
const failuresOf = (c: FlowStats | null) => (c ? c.failed + c.crashed : 0);

/**
 * Every flow the checkout defines, with its counts here. `shown` holds the
 * flows run here since start and those that take no run here, failing first,
 * then by latest run; `notYet` the flows that can run here but have not.
 */
export function flowRows(s: WorkerStatusPayload): { shown: FlowRow[]; notYet: FlowRow[] } {
  const counts = new Map((s.stats?.by_flow ?? []).map((c) => [c.flow, c]));
  const rows: FlowRow[] = s.flows.map((f) => ({
    name: f.flow,
    module: f.module,
    counts: counts.get(f.flow) ?? null,
    blocked: s.drift.includes(f.module)
      ? "older code · takes no run"
      : f.runs_on === "server"
        ? "runs on the server only"
        : null,
  }));
  for (const c of s.stats?.by_flow ?? []) {
    if (!rows.some((r) => r.name === c.flow))
      rows.push({ name: c.flow, module: null, counts: c, blocked: null });
  }
  // A running flow's latest run is now; otherwise its last completed one.
  const latest = (r: FlowRow) =>
    r.counts?.running ? s.now : (r.counts?.last_completed_at ?? (runsOf(r.counts) ? 0 : -1));
  const shown = rows
    .filter((r) => r.blocked !== null || runsOf(r.counts) > 0)
    .sort(
      (a, b) =>
        Number(failuresOf(b.counts) > 0) - Number(failuresOf(a.counts) > 0) ||
        latest(b) - latest(a) ||
        a.name.localeCompare(b.name),
    );
  const notYet = rows
    .filter((r) => r.blocked === null && runsOf(r.counts) === 0)
    .sort((a, b) => a.name.localeCompare(b.name));
  return { shown, notYet };
}

function FlowBar({ c }: { c: FlowStats }) {
  const parts: [string, number][] = [
    ["Completed", c.completed],
    ["Failed", c.failed],
    ["Crashed", c.crashed],
    ["Cancelled", c.cancelled],
    ["Running", c.running],
  ];
  const total = parts.reduce((n, [, v]) => n + v, 0);
  const title = parts
    .filter(([, v]) => v > 0)
    .map(([k, v]) => `${v} ${k.toLowerCase()}`)
    .join(" · ");
  return (
    <div className="flex h-2 overflow-hidden rounded-full bg-muted" title={title} data-testid="flow-bar">
      {total > 0
        ? parts
            .filter(([, v]) => v > 0)
            .map(([k, v]) => (
              <span key={k} className={STATE_COLOUR[k]} style={{ width: `${(v / total) * 100}%` }} />
            ))
        : null}
    </div>
  );
}

/** A flow's last run as the counts tell it: running now, or its last completed run. */
function lastRun(c: FlowStats | null): string {
  const done = c?.last_completed_at ? `last completed ${relativeTime(c.last_completed_at)}` : null;
  if (c?.running) return ["running now", done].filter(Boolean).join(" · ");
  const failed = failuresOf(c);
  if (failed) return [`${failed} failed since start`, done ?? "no completed run yet"].join(" · ");
  return done ?? "no completed run yet";
}

function Flows({ s, stale }: { s: WorkerStatusPayload; stale: boolean }) {
  const { shown, notYet } = flowRows(s);
  const runnable = s.flows.filter((f) => !s.drift.includes(f.module) && f.runs_on !== "server").length;
  return (
    <section className="rounded-lg border bg-card" aria-label="Flows">
      <CardHead
        title="Flows run here"
        aside={`since this worker started · failing first · ${runnable} of ${s.flows.length} flows can run here`}
      />
      {s.flows.length === 0 && shown.length === 0 ? (
        <p className="px-4 py-3 text-muted-foreground">This checkout defines no flow.</p>
      ) : shown.length === 0 ? (
        <p className="px-4 py-3 text-muted-foreground">No run here since this worker started.</p>
      ) : (
        <ul>
          {shown.map((r) => {
            const c = r.counts;
            const failed = failuresOf(c);
            return (
              <li
                key={r.name}
                className="flex flex-col gap-1.5 border-t px-4 py-2.5 first:border-t-0"
                data-testid="flow-row"
              >
                <div className="flex items-end justify-between gap-3">
                  <div className="flex min-w-0 flex-col">
                    <span className="truncate font-mono">{r.name}</span>
                    {r.blocked ? (
                      <span
                        className={cn(
                          "text-xs",
                          r.blocked.startsWith("older")
                            ? "text-amber-700 dark:text-amber-400"
                            : "text-muted-foreground",
                        )}
                      >
                        {r.blocked}
                      </span>
                    ) : (
                      <span
                        className={cn(
                          "text-xs",
                          failed ? "text-red-600 dark:text-red-400" : "text-muted-foreground",
                        )}
                      >
                        {lastRun(c)}
                      </span>
                    )}
                  </div>
                  <span className={cn("whitespace-nowrap font-mono", stale && "opacity-45")}>
                    {c && !r.blocked ? (
                      <>
                        {c.completed} <span className="text-muted-foreground">·</span>{" "}
                        <span className={failed ? "text-red-600 dark:text-red-400" : "text-muted-foreground"}>
                          {failed} ✗
                        </span>
                      </>
                    ) : (
                      <span className="text-muted-foreground">-</span>
                    )}
                  </span>
                </div>
                {!r.blocked && c ? (
                  <div className={cn(stale && "opacity-45")}>
                    <FlowBar c={c} />
                  </div>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
      {notYet.length > 0 ? (
        <details className="group border-t" data-testid="flows-not-run">
          <summary className="cursor-pointer px-4 py-2.5 text-muted-foreground hover:text-foreground">
            {notYet.length === 1
              ? "1 more flow can run here but has not yet"
              : `${notYet.length} more flows can run here but have not yet`}
          </summary>
          <ul className="flex flex-wrap gap-1.5 px-4 pb-3">
            {notYet.map((r) => (
              <li key={r.name} className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">
                {r.name}
              </li>
            ))}
          </ul>
        </details>
      ) : null}
    </section>
  );
}

/** The flow each processor slot last ran, remembered across polls. */
type LastRan = Map<number, { flow: string; at: number }>;

function Processors({ s, busy, lastRan }: { s: WorkerStatusPayload; busy: number | null; lastRan: LastRan }) {
  // Which run an engine holds only the server knows; without it, show the
  // engines this machine is running and nothing the server said before.
  const known = s.server_reachable && s.stats !== null;
  const bySlot = new Map<number, EngineStats>();
  if (known) for (const e of s.stats?.engines ?? []) if (!bySlot.has(e.slot)) bySlot.set(e.slot, e);
  return (
    <section className="rounded-lg border bg-card" aria-label="Processors">
      <CardHead
        title="Processors"
        aside={
          known
            ? `${busy ?? 0} of ${s.processors} busy · this machine has ${s.cpus} CPUs`
            : `${s.engines.length} engine(s) running; runs unknown until the server answers`
        }
      />
      <ul className="grid grid-cols-1 gap-2 p-3 sm:grid-cols-2">
        {Array.from({ length: s.processors }, (_, i) => i + 1).map((slot) => {
          const e = bySlot.get(slot);
          const last = lastRan.get(slot);
          return (
            <li
              key={slot}
              className={cn(
                "flex min-w-0 flex-col gap-0.5 rounded-md border px-3 py-2",
                e?.run_id ? "border-sky-300 dark:border-sky-800" : "border-dashed",
              )}
              data-testid="processor-slot"
            >
              <span className="flex items-center gap-2 font-mono text-xs">
                <span className={cn("size-1.5 rounded-full", e?.run_id ? "bg-sky-500" : "bg-border")} />P
                {slot}
                {e ? ` · ${e.module}` : ""}
              </span>
              {e?.run_id ? (
                <span className="truncate">
                  Run #{e.run_id}
                  {e.flow ? ` · ${e.flow}` : ""} · {formatDuration(e.since_secs * 1_000_000)}
                </span>
              ) : (
                <span className="truncate text-muted-foreground">
                  {e
                    ? e.status === "idle"
                      ? `Idle${last ? ` · last ran ${last.flow} ${relativeTime(last.at)}` : ""}`
                      : e.status
                    : !known && slot <= s.engines.length
                      ? "engine running · run unknown"
                      : "not started"}
                </span>
              )}
            </li>
          );
        })}
      </ul>
    </section>
  );
}

const LEVEL_DOT: Record<string, string> = {
  info: "bg-sky-500",
  warning: "bg-amber-500",
  error: "bg-red-500",
};

function Events({ s }: { s: WorkerStatusPayload }) {
  const events = s.events.slice(-20).reverse();
  return (
    <section className="rounded-lg border bg-card" aria-label="Recent activity">
      <CardHead title="Recent activity" aside="this worker's last 20 messages" />
      {events.length === 0 ? (
        <p className="px-4 py-3 text-muted-foreground">Nothing yet.</p>
      ) : (
        <ol className="max-h-[470px] overflow-auto py-1.5 max-[1100px]:max-h-[260px]">
          {events.map((e) => (
            <li
              key={`${e.at}-${e.message}`}
              className="flex gap-2.5 px-4 py-1.5"
              data-testid="worker-event"
              data-level={e.level}
            >
              <span
                className={cn("mt-1.5 size-2 flex-none rounded-full", LEVEL_DOT[e.level] ?? "bg-border")}
                title={e.level}
                data-testid="event-dot"
              />
              <div className="flex min-w-0 flex-col">
                <span
                  className={cn(
                    "break-words",
                    e.level === "warning" && "text-amber-700 dark:text-amber-400",
                    e.level === "error" && "text-red-600 dark:text-red-400",
                  )}
                >
                  {e.message}
                  {e.count > 1 ? <span className="text-muted-foreground"> ×{e.count}</span> : null}
                </span>
                <time className="font-mono text-xs text-muted-foreground">
                  {new Date(e.at / 1000).toLocaleTimeString(undefined, { hour12: false })}
                </time>
              </div>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

export function WorkerPage({ s, fetchError }: { s: WorkerStatusPayload; fetchError?: string | null }) {
  const { dark, toggle } = useTheme();
  const status = pageStatus(s);
  const stale = !s.server_reachable;
  const asOf = stale && s.stats_as_of ? ` · as of ${formatClock(s.stats_as_of)}` : "";
  const counts = (s.stats?.by_flow ?? []).reduce(
    (n, c) => ({
      completed: n.completed + c.completed,
      failed: n.failed + c.failed + c.crashed,
      finished: n.finished + c.completed + c.failed + c.crashed + c.cancelled,
    }),
    { completed: 0, failed: 0, finished: 0 },
  );
  const busy = (s.stats?.engines ?? []).filter((e) => e.run_id).length;
  // Stale engine rows would claim runs the worker may no longer hold.
  const running = s.server_reachable ? (s.stats ? busy : null) : s.engines.length;
  const git = typeof s.host.meta.git === "string" ? s.host.meta.git : null;
  const pid = typeof s.host.meta.pid === "number" ? s.host.meta.pid : null;
  const runnable = s.flows.filter((f) => !s.drift.includes(f.module) && f.runs_on !== "server").length;
  // The server names an engine's flow only while it runs one: remember it per slot.
  const lastRan = useRef<LastRan>(new Map()).current;
  if (s.server_reachable)
    for (const e of s.stats?.engines ?? [])
      if (e.run_id && e.flow) lastRan.set(e.slot, { flow: e.flow, at: s.now });
  useEffect(() => {
    document.title = `${s.name} · cereyan worker`;
  }, [s.name]);
  return (
    <div className="min-h-full">
      <header className="sticky top-0 z-10 border-b bg-background">
        <div className="frame flex h-12 items-center gap-2.5">
          <Mark />
          <nav className="flex min-w-0 items-center gap-1.5 font-semibold" aria-label="Breadcrumb">
            <span>cereyan</span>
            <span className="font-normal text-muted-foreground">›</span>
            <span className="font-medium text-muted-foreground">worker</span>
            <span className="font-normal text-muted-foreground">›</span>
            <span className="truncate">{s.name}</span>
          </nav>
          <div className="flex-1" />
          <span
            className="flex h-7 items-center gap-1.5 px-2 text-xs text-muted-foreground"
            data-testid="connection"
          >
            <span
              className={cn(
                "h-[7px] w-[7px] rounded-full",
                s.server_reachable
                  ? "bg-emerald-500 shadow-[0_0_0_3px_rgba(16,185,129,0.18)]"
                  : "animate-pulse bg-red-500",
              )}
            />
            {s.server_reachable ? "connected" : "reconnecting"}
          </span>
          <Button variant="ghost" size="icon" className="size-7" onClick={toggle} aria-label="Toggle theme">
            {dark ? <Sun className="size-4" /> : <Moon className="size-4" />}
          </Button>
          <Button variant="outline" size="sm" asChild>
            <a href={s.server_ui} target="_blank" rel="noreferrer">
              Open in server <ArrowUpRight className="size-3.5" />
            </a>
          </Button>
        </div>
      </header>

      <main className="frame flex flex-col gap-4 pt-5 pb-8">
        <section className="flex flex-col gap-3 rounded-lg border bg-card p-4" aria-label="Health">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="text-lg font-semibold">{s.name}</h1>
            <Pill status={status} />
          </div>
          <p className="text-[15px]" data-testid="health-headline">
            {workerHeadline({ status, busy: running, processors: s.processors, runnable, drift: s.drift })}
          </p>
          <div className="grid grid-cols-2 gap-x-4 gap-y-3 border-t pt-3 sm:grid-cols-3 min-[1100px]:grid-cols-6">
            <Fact label="Server" className="font-mono text-[13px]">
              {s.server.replace(/^https?:\/\//, "")}
            </Fact>
            <Fact label="Worker id">{s.worker_id ?? "not registered yet"}</Fact>
            <Fact label="Version">
              <span className="font-mono text-[13px]">{s.version}</span>
              {s.worker_id !== null ? (
                <span className="text-muted-foreground"> · same major version as the server</span>
              ) : s.state === "refused" ? (
                <span className="text-muted-foreground"> · not accepted by the server</span>
              ) : null}
            </Fact>
            <Fact
              label="Code"
              className={cn(
                s.drift.length || git?.includes("dirty") ? "text-amber-700 dark:text-amber-400" : "",
                git && "font-mono text-[13px]",
              )}
            >
              {s.drift.length ? `older code in ${s.drift.join(", ")}` : (git ?? "Not a git checkout")}
            </Fact>
            <Fact label="Last heartbeat">
              {s.last_ok_heartbeat_at ? relativeTime(s.last_ok_heartbeat_at) : "none yet"}
              {stale && s.last_ok_heartbeat_at ? " (failing)" : ""}
            </Fact>
            <Fact label="Up">
              {uptime(s.now - s.started_at)}
              {pid !== null ? ` · pid ${pid}` : ""}
            </Fact>
          </div>
          {stale ? (
            <Banner tone="bad">
              Can't reach <span className="font-mono">{s.server}</span>
              {s.failing_since
                ? ` since ${formatClock(s.failing_since)} (${relativeTime(s.failing_since).replace(" ago", "")})`
                : ""}
              .
              {` Retrying every ${s.heartbeat_secs} s. Engines keep running; their runs report when the server is back.`}
              {s.stats ? " Counts below are as of the last heartbeat." : ""}
            </Banner>
          ) : null}
          {s.state === "draining" ? (
            <Banner tone="warn">
              Draining: this worker takes no new run. Its current runs finish, then its processors stop.
              Resume it from the server's Workers tab.
            </Banner>
          ) : null}
          {s.drift.length > 0 ? (
            <Banner tone="warn">
              Code differs from the server in {s.drift.join(", ")}: this worker takes no run of{" "}
              {s.drift.length === 1 ? "that module" : "those modules"}. Update the checkout and its engines
              restart on their own; it rejoins on the next heartbeat.
            </Banner>
          ) : null}
          {s.refused.length > 0 ? (
            <Banner tone="warn">
              The server does not know {s.refused.join(", ")}: {s.refused.length === 1 ? "it" : "they"} will
              not run here.
            </Banner>
          ) : null}
          {fetchError ? (
            <Banner tone="bad">This page could not refresh: {fetchError}. Showing the last answer.</Banner>
          ) : null}
        </section>

        <div className="grid grid-cols-1 items-start gap-4 min-[900px]:grid-cols-[3fr_2fr]">
          <Processors s={s} busy={running} lastRan={lastRan} />
          <section className="grid grid-cols-3 gap-3" aria-label="Runs since start">
            <Tile
              state="Completed"
              count={s.stats ? counts.completed : null}
              note={`since start${asOf}`}
              stale={stale}
              testId="tile-completed"
            />
            <Tile
              state="Failed"
              count={s.stats ? counts.failed : null}
              note={
                <>
                  {counts.finished
                    ? `${((counts.failed / counts.finished) * 100).toFixed(1)} % of finished runs`
                    : "of finished runs"}
                  {asOf}
                </>
              }
              stale={stale}
              testId="tile-failed"
            />
            <Tile
              state="Running"
              count={running ?? s.engines.length}
              note={`of ${s.processors} processor${s.processors === 1 ? "" : "s"}`}
              testId="tile-running"
            />
          </section>
        </div>

        <div className="grid grid-cols-1 items-start gap-4 min-[1100px]:grid-cols-[3fr_2fr]">
          <Flows s={s} stale={stale} />
          <Events s={s} />
        </div>

        <HostCard w={s.host} aside="what this machine reports to the server" />

        <footer className="flex flex-wrap gap-3 text-xs text-muted-foreground">
          <span>Read-only</span>
          <span>·</span>
          <span>refreshes every {POLL_MS / 1000} s</span>
          <span>·</span>
          <a href="status.json" className="font-mono hover:underline">
            status.json
          </a>
          <span>·</span>
          <a href="healthz" className="font-mono hover:underline">
            healthz
          </a>
        </footer>
      </main>
    </div>
  );
}

/** The page with its data: a quiet placeholder until the first answer. */
export function WorkerStatusApp() {
  const { data, error } = useWorkerStatus();
  if (!data) {
    return (
      <div className="frame flex h-full items-center justify-center gap-2 text-muted-foreground">
        <Mark />
        {error ? `Could not load status.json: ${error}` : "Loading…"}
      </div>
    );
  }
  return <WorkerPage s={data} fetchError={error} />;
}
