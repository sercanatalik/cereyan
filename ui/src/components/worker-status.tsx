import { CardHead } from "@/components/ui/card";
import { cn } from "@/lib/utils";

/**
 * How a worker's status reads, shared by the server's Workers tab and the
 * worker's own status page so the two never disagree. `unreachable` is the
 * worker's view only: the server calls a silent worker Offline.
 */
export const STATUS = {
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
  unreachable: {
    label: "Server unreachable",
    pill: "bg-red-100 text-red-900 dark:bg-red-900/40 dark:text-red-200",
    dot: "bg-red-500",
  },
} as const;

export type WorkerStatus = keyof typeof STATUS;

/** How a worker's status reads: offline and draining first, then code drift. */
export function workerStatus(w: { state: string; drift: string[] }): WorkerStatus {
  if (w.state === "offline") return "offline";
  if (w.state === "draining") return "draining";
  if (w.drift.length > 0) return "drift";
  return "online";
}

export function Pill({ status }: { status: WorkerStatus }) {
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

/** Run state colours for bars and lanes. */
export const STATE_COLOUR: Record<string, string> = {
  Completed: "bg-emerald-500",
  Failed: "bg-red-500",
  Crashed: "bg-orange-500",
  Cancelled: "bg-zinc-400",
  Running: "bg-sky-500",
  Pending: "bg-sky-300",
};

/** What a worker reports about its machine: the fields the host card reads. */
export type HostFacts = {
  meta?: { [key: string]: unknown } | null;
  cpus: number;
  version: string;
  labels?: { [key: string]: unknown } | null;
  shared_paths?: string[] | null;
};

export const metaText = (w: HostFacts, key: string): string | null => {
  const v = w.meta?.[key];
  if (v == null || v === "") return null;
  if (Array.isArray(v)) return v.length ? v.join(", ") : null;
  return String(v);
};

const bytes = (n: unknown): string | null =>
  typeof n === "number" && n > 0 ? `${Math.round(n / 1024 ** 3)} GB` : null;

/** The host card: what the worker reported about its machine. */
export function HostCard({ w, aside }: { w: HostFacts; aside?: string }) {
  const memTotal = bytes(w.meta?.memory_total);
  const memFree = bytes(w.meta?.memory_available);
  const statusUrl = metaText(w, "status_url");
  const rows: [string, string | null][] = [
    ["Hostname", metaText(w, "hostname")],
    ["Address", metaText(w, "address")],
    ["Platform", metaText(w, "platform")],
    ["Architecture", [metaText(w, "arch"), metaText(w, "gpus")].filter(Boolean).join(" · ") || null],
    ["CPUs", String(w.cpus)],
    ["Memory", memTotal ? (memFree ? `${memTotal} (${memFree} free)` : memTotal) : null],
    ["Python", metaText(w, "python")],
    ["cereyan", w.version],
    ["Process", metaText(w, "pid") ? `pid ${metaText(w, "pid")}` : null],
    ["Checkout", metaText(w, "checkout")],
    ["Git", metaText(w, "git")],
    [
      "Labels",
      Object.entries(w.labels ?? {})
        .map(([k, v]) => `${k}=${v}`)
        .join(", ") || null,
    ],
    ["Shared paths", w.shared_paths?.length ? w.shared_paths.join(", ") : "none declared"],
    ["Connection", metaText(w, "connection")],
    ["Auth", metaText(w, "auth")],
    ["Status page (upstream)", statusUrl ? statusUrl.replace(/^https?:\/\//, "") : null],
  ];
  return (
    <section className="rounded-lg border bg-card" aria-label="Host">
      <CardHead title="Host" aside={aside ?? "reported at registration and on every heartbeat"} />
      <dl className="grid grid-cols-2 sm:grid-cols-3">
        {rows.map(([k, v]) => (
          <div
            key={k}
            className="min-w-0 border-t px-4 py-2.5 [&:nth-child(-n+2)]:border-t-0 sm:[&:nth-child(3)]:border-t-0"
          >
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
