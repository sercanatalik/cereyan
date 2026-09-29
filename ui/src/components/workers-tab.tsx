import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { ApiError, api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { Button } from "@/components/ui/button";
import { CardHead } from "@/components/ui/card";
import { useLiveEvent } from "@/lib/live";
import { cn, formatDuration, formatStamp, relativeTime } from "@/lib/utils";

type WorkerView = components["schemas"]["WorkerView"];
type Timeline = components["schemas"]["Timeline"];
type QueueView = components["schemas"]["QueueView"];

/** The server as a row of the list: id 0, as the timeline route takes it. */
const SERVER_ID = 0;

const STATUS: Record<string, { label: string; pill: string; dot: string }> = {
  server: { label: "Server", pill: "bg-muted text-foreground", dot: "bg-foreground" },
  online: {
    label: "Online",
    pill: "bg-emerald-100 text-emerald-900 dark:bg-emerald-900/40 dark:text-emerald-200",
    dot: "bg-emerald-500",
  },
  draining: {
    label: "Draining",
    pill: "bg-amber-100 text-amber-900 dark:bg-amber-900/40 dark:text-amber-200",
    dot: "bg-amber-500",
  },
  offline: {
    label: "Offline",
    pill: "bg-red-100 text-red-900 dark:bg-red-900/40 dark:text-red-200",
    dot: "bg-red-500",
  },
  drift: {
    label: "Older code",
    pill: "bg-amber-100 text-amber-900 dark:bg-amber-900/40 dark:text-amber-200",
    dot: "bg-amber-500",
  },
};

/** How a worker's status reads: offline and draining first, then code drift. */
export function workerStatus(w: Pick<WorkerView, "state" | "drift">): keyof typeof STATUS {
  if (w.state === "offline") return "offline";
  if (w.state === "draining") return "draining";
  if (w.drift.length > 0) return "drift";
  return "online";
}

function Pill({ status }: { status: keyof typeof STATUS }) {
  const s = STATUS[status];
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 whitespace-nowrap rounded-full px-2 py-0.5 text-xs font-medium",
        s.pill,
      )}
      data-testid="worker-status"
    >
      <span className={cn("size-1.5 rounded-full", s.dot)} />
      {s.label}
    </span>
  );
}

const meta = (w: WorkerView, key: string): string | null => {
  const v = (w.meta as Record<string, unknown>)[key];
  if (v == null || v === "") return null;
  if (Array.isArray(v)) return v.length ? v.join(", ") : null;
  return String(v);
};

const bytes = (n: unknown): string | null =>
  typeof n === "number" && n > 0 ? `${Math.round(n / 1024 ** 3)} GB` : null;

/** The host card: what the worker reported about its machine. */
function HostCard({ w }: { w: WorkerView }) {
  const memTotal = bytes((w.meta as Record<string, unknown>).memory_total);
  const memFree = bytes((w.meta as Record<string, unknown>).memory_available);
  const rows: [string, string | null][] = [
    ["Hostname", meta(w, "hostname")],
    ["Address", meta(w, "address")],
    ["Platform", meta(w, "platform")],
    ["Architecture", [meta(w, "arch"), meta(w, "gpus")].filter(Boolean).join(" · ") || null],
    ["CPUs", String(w.cpus)],
    ["Memory", memTotal ? (memFree ? `${memTotal} (${memFree} free)` : memTotal) : null],
    ["Python", meta(w, "python")],
    ["cereyan", w.version],
    ["Process", meta(w, "pid") ? `pid ${meta(w, "pid")}` : null],
    ["Checkout", meta(w, "checkout")],
    ["Git", meta(w, "git")],
    [
      "Labels",
      Object.entries(w.labels ?? {})
        .map(([k, v]) => `${k}=${v}`)
        .join(", ") || null,
    ],
    ["Shared paths", w.shared_paths?.length ? w.shared_paths.join(", ") : "none declared"],
    ["Connection", meta(w, "connection")],
    ["Auth", meta(w, "auth")],
  ];
  return (
    <section className="rounded-lg border bg-card" aria-label="Host">
      <CardHead title="Host" aside="reported at registration and on every heartbeat" />
      <dl className="grid grid-cols-3">
        {rows.map(([k, v]) => (
          <div key={k} className="min-w-0 border-t px-4 py-2.5 [&:nth-child(-n+3)]:border-t-0">
            <dt className="text-xs text-muted-foreground">{k}</dt>
            <dd
              className={cn(
                "truncate font-mono text-[13px]",
                (k === "Shared paths" && v === "none declared") || (k === "Git" && v?.includes("dirty"))
                  ? "text-amber-700 dark:text-amber-400"
                  : "",
              )}
              title={v ?? ""}
            >
              {v ?? "-"}
            </dd>
          </div>
        ))}
      </dl>
    </section>
  );
}

const STATE_COLOUR: Record<string, string> = {
  Completed: "bg-emerald-500",
  Failed: "bg-red-500",
  Crashed: "bg-orange-500",
  Cancelled: "bg-zinc-400",
  Running: "bg-sky-500",
  Pending: "bg-sky-300",
};

/** Lanes per processor over the window, and the forecast after now. */
function Schedule({ t, processors }: { t: Timeline; processors: number }) {
  const now = Date.now() * 1000;
  const start = t.since;
  const end = now + 3_600_000_000;
  const span = end - start;
  const x = (at: number) => `${(((Math.max(at, start) - start) / span) * 100).toFixed(2)}%`;
  const w = (a: number, b: number) =>
    `${(((Math.min(b, end) - Math.max(a, start)) / span) * 100).toFixed(2)}%`;
  const lanes = Math.max(processors, ...t.runs.map((r) => r.processor ?? 1));
  return (
    <section className="rounded-lg border bg-card" aria-label="Schedule" data-testid="worker-schedule">
      <CardHead title="Schedule" aside="last 6 h and the next hour; the dashed part is a forecast" />
      <div className="flex flex-col gap-1.5 px-4 py-3">
        {Array.from({ length: lanes }, (_, i) => i + 1).map((lane) => (
          <div key={lane} className="grid grid-cols-[48px_minmax(0,1fr)] items-center gap-2">
            <span className="font-mono text-xs text-muted-foreground">P{lane}</span>
            <div className="relative h-5 rounded bg-muted/60">
              <div className="absolute inset-y-[-4px] w-px bg-foreground" style={{ left: x(now) }} />
              {t.runs
                .filter((r) => (r.processor ?? 1) === lane && r.start)
                .map((r) => (
                  <Link
                    key={r.run_id}
                    to="/runs/$runId"
                    params={{ runId: String(r.run_id) }}
                    title={`${r.flow} #${r.run_id}${r.state ? ` · ${r.state}` : ""}`}
                    className={cn(
                      "absolute inset-y-0.5 rounded-sm",
                      STATE_COLOUR[r.state ?? ""] ?? "bg-sky-500",
                    )}
                    style={{ left: x(r.start as number), width: w(r.start as number, r.end ?? now) }}
                    data-testid="timeline-run"
                  />
                ))}
            </div>
          </div>
        ))}
        {t.next.length ? (
          <div className="mt-1 text-xs text-muted-foreground" data-testid="timeline-next">
            Can take next:{" "}
            {t.next
              .slice(0, 5)
              .map((r) => `${r.flow} #${r.run_id}`)
              .join(", ")}
            {t.next.length > 5 ? `, and ${t.next.length - 5} more` : ""}
          </div>
        ) : (
          <div className="mt-1 text-xs text-muted-foreground">
            Nothing in line or due within the hour for it.
          </div>
        )}
      </div>
    </section>
  );
}

function Detail({
  w,
  queue,
  onChanged,
}: {
  w: WorkerView | null;
  queue: QueueView | undefined;
  onChanged: () => void;
}) {
  const id = w?.id ?? SERVER_ID;
  const [error, setError] = useState<string | null>(null);
  const timeline = useQuery({
    queryKey: ["worker-timeline", id],
    queryFn: async () =>
      unwrap(await api.GET("/api/workers/{id}/timeline", { params: { path: { id }, query: {} } })),
    refetchInterval: 10_000,
  });
  const act = useMutation({
    mutationFn: async (what: "drain" | "resume" | "forget") => {
      if (what === "forget") {
        const r = await api.DELETE("/api/workers/{id}", { params: { path: { id } } });
        if (!r.response.ok) throw new ApiError(r.response.status, (r.error as any) ?? {});
        return null;
      }
      return unwrap(
        what === "drain"
          ? await api.POST("/api/workers/{id}/drain", { params: { path: { id } } })
          : await api.POST("/api/workers/{id}/resume", { params: { path: { id } } }),
      );
    },
    onSuccess: () => {
      setError(null);
      onChanged();
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
  });
  const host = w?.name ?? "server";
  const engines = (queue?.processors.items ?? []).filter((e) => e.host === host);
  const status = w ? workerStatus(w) : "server";
  const processors = w?.processors ?? queue?.processors.count ?? 1;
  return (
    <div className="flex min-w-0 flex-col gap-4" data-testid="worker-detail">
      <section className="flex flex-col gap-3 rounded-lg border bg-card p-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="flex flex-col gap-0.5">
            <div className="flex items-center gap-2">
              <h2 className="text-lg font-semibold">{host}</h2>
              <Pill status={status} />
            </div>
            <span className="text-muted-foreground">
              {w
                ? "Takes any run whose code matches, after the server's processors are full."
                : 'The machine running cereyan serve. Flows marked runs_on="server" only run here.'}
            </span>
          </div>
          {w ? (
            <div className="flex gap-2">
              {w.state === "online" ? (
                <Button size="sm" variant="outline" onClick={() => act.mutate("drain")}>
                  Drain
                </Button>
              ) : null}
              {w.state === "draining" ? (
                <Button size="sm" variant="outline" onClick={() => act.mutate("resume")}>
                  Resume
                </Button>
              ) : null}
              {w.state === "offline" ? (
                <Button size="sm" variant="outline" onClick={() => act.mutate("forget")}>
                  Forget worker
                </Button>
              ) : null}
            </div>
          ) : null}
        </div>
        <div className="grid grid-cols-4 gap-3 text-xs">
          <div className="flex flex-col">
            <span className="text-muted-foreground">Processors</span>
            <span className="font-mono text-sm">
              {engines.filter((e) => e.run_id).length} busy / {processors}
              {w ? ` of ${w.cpus} CPUs` : ""}
            </span>
          </div>
          <div className="flex flex-col">
            <span className="text-muted-foreground">Version</span>
            <span className="font-mono text-sm">{w?.version ?? "this server"}</span>
          </div>
          <div className="flex flex-col">
            <span className="text-muted-foreground">Code</span>
            <span
              className={cn("font-mono text-sm", w?.drift.length ? "text-amber-700 dark:text-amber-400" : "")}
            >
              {w ? (meta(w, "git") ?? "not a git checkout") : "reference"}
            </span>
          </div>
          <div className="flex flex-col">
            <span className="text-muted-foreground">Last heartbeat</span>
            <span className="text-sm">{w ? relativeTime(w.last_seen_at) : "-"}</span>
          </div>
        </div>
        {w?.state === "offline" ? (
          <div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-red-900 dark:border-red-900 dark:bg-red-950/40 dark:text-red-200">
            Offline since {formatStamp(w.last_seen_at)}. Runs it held were crashed and rerun on other hosts by
            their own heartbeats. Forget it if it is not coming back.
          </div>
        ) : null}
        {w?.state === "draining" ? (
          <div className="rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-amber-900 dark:border-amber-800 dark:bg-amber-950/30 dark:text-amber-200">
            Draining: it takes no new run. Its current runs finish, then its processors stop.
          </div>
        ) : null}
        {w && w.drift.length > 0 && w.state !== "offline" ? (
          <div className="rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-amber-900 dark:border-amber-800 dark:bg-amber-950/30 dark:text-amber-200">
            Code differs from the server in {w.drift.join(", ")}: it takes no run of{" "}
            {w.drift.length === 1 ? "that module" : "those modules"}. Update the checkout and its engines
            restart on their own; it rejoins on the next heartbeat.
          </div>
        ) : null}
        {error ? <div className="text-destructive">{error}</div> : null}
      </section>
      {w ? <HostCard w={w} /> : null}
      {timeline.data ? <Schedule t={timeline.data} processors={processors} /> : null}
      <section className="rounded-lg border bg-card" aria-label="Now">
        <CardHead title="Now" aside={w ? `${w.flows} flows can run here` : undefined} />
        {engines.length === 0 ? (
          <p className="px-4 py-3 text-muted-foreground">No engine is running here.</p>
        ) : (
          <ul>
            {engines.map((e) => (
              <li
                key={e.id}
                className="flex items-center justify-between border-t px-4 py-2 first:border-t-0"
              >
                <span className="font-mono text-xs">
                  P{e.slot} · {e.module}
                </span>
                {e.run_id ? (
                  <Link to="/runs/$runId" params={{ runId: String(e.run_id) }} className="hover:underline">
                    Run #{e.run_id} · {formatDuration(e.since_secs * 1_000_000)}
                  </Link>
                ) : (
                  <span className="text-muted-foreground">{e.status}</span>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

/** Queue › Workers: the server and every registered worker, and one in detail. */
export function WorkersTab({ queue }: { queue: QueueView | undefined }) {
  const client = useQueryClient();
  const [selected, setSelected] = useState<number>(SERVER_ID);
  const workers = useQuery({
    queryKey: ["workers"],
    queryFn: async () => unwrap(await api.GET("/api/workers")),
    refetchInterval: 5000,
  });
  useLiveEvent("worker.updated", () => client.invalidateQueries({ queryKey: ["workers"] }));
  const list = workers.data ?? [];
  const current = list.find((w) => w.id === selected) ?? null;
  const online = list.filter((w) => w.state === "online").length;
  const hosts = queue?.processors.hosts ?? [];
  const busy = hosts.reduce((n, h) => n + h.busy, 0);
  const total = hosts.reduce((n, h) => n + h.count, 0);
  const attention = list.filter((w) => w.state === "offline" || w.drift.length > 0).length;
  const origin = typeof window !== "undefined" ? window.location.origin : "https://cereyan.example";
  return (
    <div className="flex flex-col gap-4" data-testid="workers-tab">
      <div className="grid grid-cols-3 gap-3">
        <div className="rounded-lg border bg-card px-4 py-3">
          <div className="text-xs text-muted-foreground">Processors, all machines</div>
          <div className="font-mono text-xl">
            {busy} / {total} busy
          </div>
        </div>
        <div className="rounded-lg border bg-card px-4 py-3">
          <div className="text-xs text-muted-foreground">Workers online</div>
          <div className="font-mono text-xl">
            {online} of {list.length}
          </div>
        </div>
        <div
          className={cn(
            "rounded-lg border px-4 py-3",
            attention ? "border-amber-300 bg-amber-50 dark:border-amber-800 dark:bg-amber-950/30" : "bg-card",
          )}
        >
          <div className="text-xs text-muted-foreground">Needs attention</div>
          <div className="text-xl">
            {attention ? `${attention} worker${attention === 1 ? "" : "s"}` : "nothing"}
          </div>
        </div>
      </div>
      <div className="grid grid-cols-5 gap-4">
        <section className="col-span-2 self-start rounded-lg border bg-card" aria-label="Registered">
          <CardHead title="Registered" aside="offline after 3 missed heartbeats" />
          <ul>
            {[null, ...list].map((w) => {
              const id = w?.id ?? SERVER_ID;
              const on = id === selected;
              return (
                <li key={id} className="border-t first:border-t-0">
                  <button
                    type="button"
                    onClick={() => setSelected(id)}
                    aria-pressed={on}
                    className={cn(
                      "flex w-full flex-col gap-1 px-4 py-2.5 text-left hover:bg-muted/50",
                      on && "bg-muted shadow-[inset_3px_0_0_var(--foreground)]",
                    )}
                    data-testid="worker-row"
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span className="flex min-w-0 flex-col">
                        <span className="truncate font-semibold">{w?.name ?? "server"}</span>
                        <span className="truncate font-mono text-xs text-muted-foreground">
                          {w
                            ? [meta(w, "hostname"), meta(w, "address")].filter(Boolean).join(" · ")
                            : "this machine"}
                        </span>
                      </span>
                      <Pill status={w ? workerStatus(w) : "server"} />
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {w
                        ? `${w.running} busy / ${w.processors} · ${relativeTime(w.last_seen_at)}`
                        : `${hosts.find((h) => h.host === "server")?.busy ?? 0} busy / ${queue?.processors.count ?? 1}`}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
          <div className="flex flex-col gap-2 border-t bg-muted/30 px-4 py-3">
            <span className="font-semibold">Add a worker</span>
            <span className="text-xs text-muted-foreground">
              On a machine with its own checkout of this project and the same CA in its trust store:
            </span>
            <pre className="overflow-x-auto rounded-md bg-muted px-3 py-2 font-mono text-xs">
              {`cereyan worker ./pipelines \\\n  --host ${origin} \\\n  --token-file /etc/cereyan/token \\\n  --processors 2`}
            </pre>
            <span className="text-xs text-muted-foreground">
              The worker connects out and opens no port. It appears here on its first heartbeat.
            </span>
          </div>
        </section>
        <div className="col-span-3 min-w-0">
          <Detail
            key={selected}
            w={current}
            queue={queue}
            onChanged={() => {
              client.invalidateQueries({ queryKey: ["workers"] });
              client.invalidateQueries({ queryKey: ["queue"] });
            }}
          />
        </div>
      </div>
    </div>
  );
}
