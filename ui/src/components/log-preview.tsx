import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { X } from "lucide-react";
import { useEffect, useRef } from "react";
import { api, type Log, type StateType, unwrap } from "@/api/client";
import { StateDot } from "@/components/state-badge";
import { buttonVariants } from "@/components/ui/button";
import { useLiveEvent } from "@/lib/live";
import { cn, formatDuration, formatStamp, levelName } from "@/lib/utils";

/** Pages fetched at most, a thousand lines each, as the run page's Logs tab does. */
const MAX_PAGES = 20;
const TERMINAL = new Set<StateType>(["Completed", "Failed", "Cancelled", "Crashed"]);

const LEVEL_TEXT: Record<string, string> = {
  DEBUG: "text-muted-foreground",
  INFO: "text-sky-700 dark:text-sky-300",
  WARNING: "text-amber-700 dark:text-amber-300",
  ERROR: "text-red-700 dark:text-red-300",
  CRITICAL: "font-semibold text-red-800 dark:text-red-200",
};

const clockWithSeconds = (micros: number) =>
  new Date(micros / 1000).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });

/**
 * All of a run's log lines, cached under the run page's key for an unfiltered
 * Logs tab, so opening the run afterwards shows them at once.
 */
function useRunLogLines(runId: number) {
  const client = useQueryClient();
  useLiveEvent("log.appended", (data) => {
    if (data?.run_id === runId) client.invalidateQueries({ queryKey: ["logs", runId] });
  });
  return useQuery({
    queryKey: ["logs", runId, null, "", ""],
    queryFn: async () => {
      const lines: Log[] = [];
      let after = 0;
      for (let page = 0; page < MAX_PAGES; page++) {
        const { items, next_cursor } = unwrap(
          await api.GET("/api/runs/{id}/logs", {
            params: { path: { id: runId }, query: { after, limit: 1000, level: "", search: "" } as any },
          }),
        );
        lines.push(...items);
        if (!next_cursor) break;
        after = next_cursor;
      }
      return lines;
    },
  });
}

/**
 * One run's logs inside the dashboard's Flows card: its state, name, flow and
 * timing, then its lines in a box that scrolls past 240 px. A live run's box
 * follows new lines.
 */
export function LogPreview({ runId, onClose }: { runId: number; onClose: () => void }) {
  const run = useQuery({
    queryKey: ["run", runId],
    queryFn: async () => unwrap(await api.GET("/api/runs/{id}", { params: { path: { id: runId } } })),
  });
  const logs = useRunLogLines(runId);
  const lines = logs.data ?? [];
  const r = run.data;
  const live = !!r && !TERMINAL.has(r.state.type);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    // Scroll the box, not the page, so following never moves the dashboard.
    if (live && lines.length && box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [live, lines.length]);
  const timing = r
    ? [
        r.start_time ? `started ${formatStamp(r.start_time)}` : null,
        r.total_run_time != null ? `took ${formatDuration(r.total_run_time)}` : null,
      ]
        .filter(Boolean)
        .join(" · ")
    : "";
  return (
    <div className="border-t bg-muted/30" aria-live="polite" data-testid="log-preview" data-run-id={runId}>
      <div className="flex flex-wrap items-center gap-x-3.5 gap-y-2 border-b px-4 py-3">
        {r ? <StateDot type={r.state.type} title={r.state.name} /> : null}
        <span className="font-semibold">{r?.name ?? `Run ${runId}`}</span>
        {r ? (
          <span className="text-xs text-muted-foreground">
            {r.state.name} · {r.project}/{r.flow_name}
            {timing ? ` · ${timing}` : ""}
          </span>
        ) : null}
        <div className="ml-auto flex gap-2">
          <Link
            to="/runs/$runId"
            params={{ runId: String(runId) }}
            search={{ tab: "logs" }}
            className={buttonVariants({ variant: "outline", size: "sm" })}
          >
            Open run
          </Link>
          <button
            type="button"
            aria-label="Close logs"
            onClick={onClose}
            className={buttonVariants({ variant: "outline", size: "icon-sm" })}
          >
            <X />
          </button>
        </div>
      </div>
      <div
        ref={box}
        className="max-h-60 overflow-auto py-1.5 font-mono text-xs leading-5"
        data-testid="log-preview-lines"
      >
        {lines.map((line) => {
          const level = levelName(line.level);
          return (
            <div
              key={line.id}
              data-level={level}
              className={cn(
                "flex gap-3 px-4",
                line.level >= 40 && "bg-destructive/10 dark:bg-destructive/20",
                line.level === 30 && "bg-amber-500/10",
              )}
            >
              <span className="shrink-0 text-muted-foreground">{clockWithSeconds(line.timestamp)}</span>
              <span className={cn("w-16 shrink-0", LEVEL_TEXT[level])}>{level}</span>
              <span className="min-w-0 whitespace-pre-wrap break-words">{line.message}</span>
            </div>
          );
        })}
        {lines.length === 0 ? (
          <div className="px-4 py-2 text-muted-foreground">{logs.isLoading ? "Loading logs" : "No logs"}</div>
        ) : null}
      </div>
    </div>
  );
}
