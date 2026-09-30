import { ArrowUpRight, Moon, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { Mark, useTheme } from "@/components/mark";
import { Button } from "@/components/ui/button";
import { CardHead } from "@/components/ui/card";
import { HostCard, Pill, STATE_COLOUR, type WorkerStatus, workerStatus } from "@/components/worker-status";
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

/** The worker's status as its own page reads it: not reaching the server comes first. */
export function pageStatus(s: WorkerStatusPayload): WorkerStatus {
  if (!s.server_reachable) return "unreachable";
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
    <div className="flex min-w-0 flex-col">
      <span className="text-muted-foreground">{label}</span>
      <span className={cn("truncate text-sm", className)}>{children}</span>
    </div>
  );
}

function Tile({
  label,
  value,
  note,
  alert,
  stale,
  testId,
}: {
  label: string;
  value: React.ReactNode;
  note: React.ReactNode;
  alert?: boolean;
  stale?: boolean;
  testId?: string;
}) {
  return (
    <div
      className={cn(
        "rounded-lg border px-4 py-3",
        alert ? "border-red-200 bg-red-50 dark:border-red-900 dark:bg-red-950/40" : "bg-card",
      )}
      data-testid={testId}
      data-alert={alert ? "true" : undefined}
    >
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className={cn("font-mono text-xl", stale && "opacity-45")}>{value}</div>
      <div className="text-xs text-muted-foreground">{note}</div>
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

/** Every flow the checkout defines, with its counts here; busiest first, then the rest. */
function flowRows(s: WorkerStatusPayload): FlowRow[] {
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
  const total = (r: FlowRow) =>
    r.counts
      ? r.counts.completed + r.counts.failed + r.counts.crashed + r.counts.cancelled + r.counts.running
      : 0;
  return rows.sort(
    (a, b) =>
      Number(a.blocked !== null) - Number(b.blocked !== null) ||
      total(b) - total(a) ||
      a.name.localeCompare(b.name),
  );
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

function Flows({ s, stale }: { s: WorkerStatusPayload; stale: boolean }) {
  const rows = flowRows(s);
  const runnable = rows.filter((r) => r.blocked === null && r.module).length;
  return (
    <section className="rounded-lg border bg-card" aria-label="Flows">
      <CardHead
        title="Flows"
        aside={`runs here since start · ${runnable} of ${s.flows.length} flows can run here`}
      />
      {rows.length === 0 ? (
        <p className="px-4 py-3 text-muted-foreground">This checkout defines no flow.</p>
      ) : (
        <ul>
          {rows.map((r) => {
            const c = r.counts;
            const failed = c ? c.failed + c.crashed : 0;
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
                      <span className="text-xs text-muted-foreground">
                        {[
                          c?.running ? "running" : null,
                          c?.last_completed_at
                            ? `last ✓ ${relativeTime(c.last_completed_at)}`
                            : "no completed run yet",
                        ]
                          .filter(Boolean)
                          .join(" · ")}
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
                {!r.blocked ? (
                  <div className={cn(stale && "opacity-45")}>
                    {c ? <FlowBar c={c} /> : <div className="h-2 rounded-full bg-muted" />}
                  </div>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function Now({ s }: { s: WorkerStatusPayload }) {
  // Which run an engine holds only the server knows; without it, show the
  // engines this machine is running and nothing the server said before.
  const known = s.server_reachable && s.stats !== null;
  const bySlot = new Map<number, EngineStats>();
  if (known) for (const e of s.stats?.engines ?? []) if (!bySlot.has(e.slot)) bySlot.set(e.slot, e);
  return (
    <section className="rounded-lg border bg-card" aria-label="Now">
      <CardHead
        title="Now"
        aside={
          known
            ? "one engine per processor"
            : `${s.engines.length} engine(s) running; runs unknown until the server answers`
        }
      />
      <ul>
        {Array.from({ length: s.processors }, (_, i) => i + 1).map((slot) => {
          const e = bySlot.get(slot);
          return (
            <li
              key={slot}
              className="flex flex-wrap items-center justify-between gap-x-3 gap-y-0.5 border-t px-4 py-2 first:border-t-0"
            >
              <span className="flex items-center gap-2 font-mono text-xs">
                <span className={cn("size-1.5 rounded-full", e?.run_id ? "bg-sky-500" : "bg-border")} />P
                {slot}
                {e ? ` · ${e.module}` : ""}
              </span>
              {e?.run_id ? (
                <span>
                  Run #{e.run_id}
                  {e.flow ? ` · ${e.flow}` : ""} · {formatDuration(e.since_secs * 1_000_000)}
                </span>
              ) : (
                <span className="text-muted-foreground">
                  {e
                    ? e.status
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

function Events({ s }: { s: WorkerStatusPayload }) {
  const events = s.events.slice(-20).reverse();
  return (
    <section className="rounded-lg border bg-card" aria-label="Events">
      <CardHead title="Events" aside="this worker's last 20 messages" />
      {events.length === 0 ? (
        <p className="px-4 py-3 text-muted-foreground">Nothing yet.</p>
      ) : (
        <ul className="max-h-[470px] overflow-auto py-1.5 font-mono text-xs leading-[18px] max-[1100px]:max-h-[260px]">
          {events.map((e) => (
            <li
              key={`${e.at}-${e.message}`}
              className={cn(
                "grid grid-cols-[64px_minmax(0,1fr)] gap-2 px-4 py-0.5",
                e.level === "warning" && "text-amber-700 dark:text-amber-400",
                e.level === "error" && "text-red-600 dark:text-red-400",
              )}
              data-testid="worker-event"
              data-level={e.level}
            >
              <time className="text-muted-foreground">
                {new Date(e.at / 1000).toLocaleTimeString(undefined, { hour12: false })}
              </time>
              <span>
                {e.message}
                {e.count > 1 ? <span className="text-muted-foreground"> ×{e.count}</span> : null}
              </span>
            </li>
          ))}
        </ul>
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
        <section className="flex flex-col gap-3 rounded-lg border bg-card p-4">
          <div className="flex flex-col gap-0.5">
            <div className="flex flex-wrap items-center gap-2">
              <h1 className="text-lg font-semibold">{s.name}</h1>
              <Pill status={status} />
            </div>
            <span className="text-muted-foreground">
              {s.worker_id !== null ? (
                <>
                  Registered with <span className="font-mono">{s.server}</span> as worker {s.worker_id}
                </>
              ) : (
                <>
                  Registering with <span className="font-mono">{s.server}</span>
                </>
              )}
              {" · takes any run whose code matches, after the server's processors are full."}
            </span>
          </div>
          <div className="grid grid-cols-2 gap-3 text-xs sm:grid-cols-4">
            <Fact label="Processors" className="font-mono">
              {running ?? "?"} busy / {s.processors} of {s.cpus} CPUs
            </Fact>
            <Fact label="Version" className="font-mono">
              {s.version}
            </Fact>
            <Fact
              label="Code"
              className={cn(
                "font-mono",
                s.drift.length || git?.includes("dirty") ? "text-amber-700 dark:text-amber-400" : "",
              )}
            >
              {git ?? "not a git checkout"}
            </Fact>
            <Fact label="Last heartbeat">
              {s.last_ok_heartbeat_at ? relativeTime(s.last_ok_heartbeat_at) : "none yet"}
              {stale && s.last_ok_heartbeat_at ? " (failing)" : ""}
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

        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Tile
            label="Completed"
            value={s.stats ? counts.completed : "-"}
            note={`since start${asOf}`}
            stale={stale}
            testId="tile-completed"
          />
          <Tile
            label="Failed"
            value={s.stats ? counts.failed : "-"}
            note={
              <>
                {counts.finished
                  ? `${((counts.failed / counts.finished) * 100).toFixed(1)} % of finished runs`
                  : "of finished runs"}
                {asOf}
              </>
            }
            alert={counts.failed > 0}
            stale={stale}
            testId="tile-failed"
          />
          <Tile
            label="Running"
            value={running ?? s.engines.length}
            note={`of ${s.processors} processor${s.processors === 1 ? "" : "s"}`}
            testId="tile-running"
          />
          <Tile
            label="Up"
            value={uptime(s.now - s.started_at)}
            note={`since ${formatClock(s.started_at)}${typeof s.host.meta.pid === "number" ? ` · pid ${s.host.meta.pid}` : ""}`}
            testId="tile-up"
          />
        </div>

        <div className="grid grid-cols-1 items-start gap-4 min-[1100px]:grid-cols-[3fr_2fr]">
          <div className="flex min-w-0 flex-col gap-4">
            <Flows s={s} stale={stale} />
            <Now s={s} />
          </div>
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
