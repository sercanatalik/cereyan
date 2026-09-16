import { useQuery, useQueryClient } from "@tanstack/react-query";
import { X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { api, type Log, unwrap } from "@/api/client";
import { Input, Select } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { useLiveEvent } from "@/lib/live";
import { cn, formatTime, levelName } from "@/lib/utils";

/** Pages fetched at most per load, a thousand lines each. */
const MAX_PAGES = 20;

const LEVEL_TEXT: Record<string, string> = {
  DEBUG: "text-muted-foreground",
  INFO: "text-sky-700 dark:text-sky-300",
  WARNING: "text-amber-700 dark:text-amber-300",
  ERROR: "text-red-700 dark:text-red-300",
  CRITICAL: "font-semibold text-red-800 dark:text-red-200",
};

const MIN_LEVELS = [
  { value: "", label: "All levels" },
  { value: "INFO", label: "Info and above" },
  { value: "WARNING", label: "Warning and above" },
  { value: "ERROR", label: "Error and above" },
];

interface LogFilter {
  level: string;
  search: string;
}

/**
 * The lines of a run, or of one task run within it, matching the filter.
 * Refetched whenever the live stream says the run logged something.
 */
function useLogLines(runId: number, taskRunId: number | undefined, filter: LogFilter) {
  const client = useQueryClient();
  useLiveEvent("log.appended", (data) => {
    if (data?.run_id === runId) client.invalidateQueries({ queryKey: ["logs", runId] });
  });
  return useQuery({
    queryKey: ["logs", runId, taskRunId ?? null, filter.level, filter.search],
    queryFn: async () => {
      const path = taskRunId === undefined ? "/api/runs/{id}/logs" : "/api/task-runs/{id}/logs";
      const id = taskRunId ?? runId;
      const lines: Log[] = [];
      let after = 0;
      for (let page = 0; page < MAX_PAGES; page++) {
        const { items, next_cursor } = unwrap(
          await api.GET(
            path as any,
            { params: { path: { id }, query: { after, limit: 1000, ...filter } } } as any,
          ),
        ) as { items: Log[]; next_cursor?: number | null };
        lines.push(...items);
        if (!next_cursor) break;
        after = next_cursor;
      }
      return lines;
    },
  });
}

/** Keeps the end of the list in view while `enabled`, each time `count` changes. */
function useStickToEnd(count: number, enabled: boolean) {
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (enabled && count > 0) end.current?.scrollIntoView({ block: "end" });
  }, [count, enabled]);
  return end;
}

function TaskChip({ label, onClear }: { label: string; onClear?: () => void }) {
  return (
    <span
      className="inline-flex h-6 items-center gap-1.5 rounded-[5px] border bg-muted pr-1 pl-2 text-xs font-medium"
      data-testid="log-task-chip"
    >
      <span className="font-normal text-muted-foreground">task</span>
      {label}
      {onClear ? (
        <button
          type="button"
          aria-label="Show all tasks"
          onClick={onClear}
          className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          <X className="size-3" />
        </button>
      ) : null}
    </span>
  );
}

function LogTable({ lines }: { lines: Log[] }) {
  return (
    <table className="w-full">
      <tbody>
        {lines.map((line) => {
          const level = levelName(line.level);
          return (
            <tr
              key={line.id}
              className={cn(
                "align-top hover:bg-accent/40",
                line.level >= 40 && "bg-destructive/5 dark:bg-destructive/10",
              )}
            >
              <td className="px-3 py-0.5 whitespace-nowrap text-muted-foreground">
                {formatTime(line.timestamp)}
              </td>
              <td className={cn("px-2 py-0.5 whitespace-nowrap", LEVEL_TEXT[level])}>{level}</td>
              <td className="px-2 py-0.5 text-muted-foreground">{line.logger}</td>
              <td className="w-full px-2 py-0.5 whitespace-pre-wrap">{line.message}</td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

/**
 * A run's log lines with a minimum level, a text search, and Follow, which keeps
 * the newest line in view while the panel is `active`. With `taskRunId` the lines
 * are that task run's, shown as a chip that `onClearTask` removes.
 */
export function RunLogs({
  runId,
  taskRunId,
  taskLabel,
  onClearTask,
  active,
}: {
  runId: number;
  taskRunId?: number;
  taskLabel?: string;
  onClearTask?: () => void;
  active?: boolean;
}) {
  const [filter, setFilter] = useState<LogFilter>({ level: "", search: "" });
  const [follow, setFollow] = useState(true);
  const query = useLogLines(runId, taskRunId, filter);
  const lines = query.data ?? [];
  const end = useStickToEnd(lines.length, follow && !!active);
  return (
    <div className="flex h-full flex-col gap-2">
      <div className="flex items-center gap-2">
        <Select
          aria-label="Minimum level"
          className="w-40"
          value={filter.level}
          onChange={(e) => setFilter({ ...filter, level: e.target.value })}
        >
          {MIN_LEVELS.map((l) => (
            <option key={l.value} value={l.value}>
              {l.label}
            </option>
          ))}
        </Select>
        <Input
          placeholder="Search logs"
          className="max-w-xs"
          value={filter.search}
          onChange={(e) => setFilter({ ...filter, search: e.target.value })}
        />
        {taskLabel ? <TaskChip label={taskLabel} onClear={onClearTask} /> : null}
        <span className="ml-auto flex items-center gap-2 text-xs text-muted-foreground">
          Follow
          <Switch checked={follow} onCheckedChange={setFollow} aria-label="Follow" />
        </span>
      </div>
      <div
        className="min-h-64 flex-1 overflow-auto rounded-lg border bg-card py-1 font-mono text-xs"
        data-testid="log-list"
      >
        {lines.length ? (
          <LogTable lines={lines} />
        ) : (
          <div className="p-4 text-muted-foreground">{query.isLoading ? "Loading logs" : "No logs"}</div>
        )}
        <div ref={end} />
      </div>
    </div>
  );
}
