/**
 * The Flows page's words: the quick filters, the subtitle's attention count,
 * and a flow's health check as the row's sub-line reads it.
 */
import type { Flow } from "@/api/client";

const latest = (f: Pick<Flow, "recent_runs">) => f.recent_runs[0]?.[1];

/** The quick-filter chips, in the order the page shows them. */
export const QUICK_FILTERS = [
  { value: "all", label: "All", match: () => true },
  {
    value: "failing",
    label: "Failing",
    match: (f: Flow) => latest(f) === "Failed" || latest(f) === "Crashed",
  },
  { value: "scheduled", label: "Scheduled", match: (f: Flow) => f.schedules.length > 0 },
  { value: "waiting", label: "Waiting for input", match: (f: Flow) => latest(f) === "Paused" },
  { value: "never", label: "Never run", match: (f: Flow) => f.recent_runs.length === 0 },
] as const;

export type QuickFilter = (typeof QUICK_FILTERS)[number]["value"];

/** Whether a flow passes a quick filter. */
export function matchesQuickFilter(f: Flow, filter: QuickFilter): boolean {
  return (QUICK_FILTERS.find((q) => q.value === filter) ?? QUICK_FILTERS[0]).match(f);
}

/**
 * How many flows need the user, for the subtitle: "1 is failing, 2 are
 * waiting for input". Null when none does, so a quiet scope stays quiet.
 */
export function flowsAttention(flows: Flow[]): string | null {
  const failing = flows.filter((f) => matchesQuickFilter(f, "failing")).length;
  const waiting = flows.filter((f) => matchesQuickFilter(f, "waiting")).length;
  const parts = [
    failing ? `${failing} ${failing === 1 ? "is" : "are"} failing` : null,
    waiting ? `${waiting} ${waiting === 1 ? "is" : "are"} waiting for input` : null,
  ].filter((p): p is string => p !== null);
  return parts.length ? parts.join(", ") : null;
}

/** A flow's health check in words, and whether it reads as a failure. */
export function healthLine(health: Flow["health"]): { text: string; failing: boolean } | null {
  if (!health) return null;
  const reason = health.reasons.join("; ");
  if (health.status === "FAIL")
    return { text: `Health check failing${reason ? `: ${reason}` : ""}`, failing: true };
  if (health.status === "WARN")
    return { text: `Health check warning${reason ? `: ${reason}` : ""}`, failing: false };
  return { text: "Health check passing", failing: false };
}
