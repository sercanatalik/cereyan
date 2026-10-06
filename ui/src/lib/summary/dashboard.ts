// What the dashboard derives from data it already fetches: the Flows card's
// rows, their status lines, the preview's default run, and the idle processors.

import type { Flow, Run } from "@/api/client";
import type { components } from "@/api/schema";
import { groupOf } from "@/lib/groups";
import { formatDuration, formatFire } from "@/lib/utils";

type Processors = components["schemas"]["Processors"];

/** The Flows card lists at most this many flows. */
export const FLOWS_LIMIT = 8;

const failing = (type: string | undefined) => type === "Failed" || type === "Crashed";

/** 0 for a flow whose latest run failed or crashed, 1 for one waiting for input, 2 otherwise. */
function urgency(flow: Flow): number {
  const type = flow.recent_runs[0]?.[1];
  if (failing(type)) return 0;
  return type === "Paused" ? 1 : 2;
}

/** Flows in scope with at least one run: failing first, then paused, then newest latest run. */
export function dashboardFlows(flows: Flow[], project?: string, group?: string): Flow[] {
  return flows
    .filter((f) => (!project || f.project === project) && (!group || groupOf(f) === group))
    .filter((f) => f.recent_runs.length > 0)
    .sort((a, b) => urgency(a) - urgency(b) || b.recent_runs[0][0] - a.recent_runs[0][0])
    .slice(0, FLOWS_LIMIT);
}

/** The newest Failed or Crashed run among `flows`, which the log preview opens on. */
export function defaultPreviewRun(flows: Flow[]): number | null {
  let newest: number | null = null;
  for (const f of flows)
    for (const [id, type] of f.recent_runs)
      if (failing(type) && (newest === null || id > newest)) newest = id;
  return newest;
}

/** A fire within the coming week by weekday and time ("Wed 06:30"), a later one with its date. */
function nextFire(micros: number): string {
  if (micros / 1000 - Date.now() > 6 * 86_400_000) return formatFire(micros);
  return new Date(micros / 1000).toLocaleString(undefined, {
    weekday: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** "Failed · 41 ms · next Wed 06:30": the latest run, then the next fire of an active schedule. */
export function flowStatusLine(flow: Flow): { text: string; failing: boolean } {
  const latest = flow.recent_runs[0];
  const parts: string[] = [];
  if (latest) {
    const [, , name, duration] = latest;
    parts.push(duration == null ? name : `${name} · ${formatDuration(duration)}`);
  }
  const nexts = flow.schedules
    .filter((s) => s.active)
    .map((s) => s.next_fire)
    .filter((n): n is number => n != null);
  if (nexts.length) parts.push(`next ${nextFire(Math.min(...nexts))}`);
  return { text: parts.join(" · "), failing: failing(latest?.[1]) };
}

/** Why an upcoming run is due: its schedule, a retry, a backfill, or a delayed start. */
export function upcomingReason(run: Run): string {
  if (run.state.name === "AwaitingRetry") return "retry";
  if (run.created_by?.startsWith("schedule")) return "schedule";
  if (run.created_by?.startsWith("backfill")) return "backfill";
  return "delayed start";
}

/** Processors free to take a run, across the server and every worker that is not offline. */
export function idleProcessors(processors: Processors): number {
  const hosts = (processors.hosts ?? []).filter((h) => h.state !== "offline");
  if (hosts.length) return hosts.reduce((n, h) => n + Math.max(0, h.count - h.busy), 0);
  const busy = processors.items.filter((e) => e.status === "running").length;
  return Math.max(0, processors.count - busy);
}
