import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Download, X } from "lucide-react";
import { Fragment, useEffect, useRef, useState } from "react";
import { api, type Log, unwrap } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { useLiveEvent } from "@/lib/live";
import { cn, levelName } from "@/lib/utils";

/** Pages fetched at most per load, a thousand lines each. */
const MAX_PAGES = 20;

const LEVEL_TEXT: Record<string, string> = {
  DEBUG: "text-muted-foreground",
  INFO: "text-sky-700 dark:text-sky-300",
  WARNING: "text-amber-700 dark:text-amber-300",
  ERROR: "text-red-700 dark:text-red-300",
  CRITICAL: "font-semibold text-red-800 dark:text-red-200",
};

/**
 * The level chips. Each counts its own level only; Error takes CRITICAL too.
 * Debug shows only when the log has debug lines.
 */
const LEVEL_CHIPS = [
  { value: "", label: "All", match: () => true },
  { value: "DEBUG", label: "Debug", match: (l: number) => l < 20 },
  { value: "INFO", label: "Info", match: (l: number) => l >= 20 && l < 30 },
  { value: "WARNING", label: "Warning", match: (l: number) => l >= 30 && l < 40 },
  { value: "ERROR", label: "Error", match: (l: number) => l >= 40 },
];

/**
 * Every line of a run, or of one task run within it, matching `search`.
 * Refetched whenever the live stream says the run logged something. All
 * levels are fetched (the level slot of the key stays "") so the chips can
 * count each level; the dashboard's log preview shares this key.
 */
export function useLogLines(runId: number, taskRunId: number | undefined, search = "") {
  const client = useQueryClient();
  useLiveEvent("log.appended", (data) => {
    if (data?.run_id === runId) client.invalidateQueries({ queryKey: ["logs", runId] });
  });
  return useQuery({
    queryKey: ["logs", runId, taskRunId ?? null, "", search],
    queryFn: async () => {
      const path = taskRunId === undefined ? "/api/runs/{id}/logs" : "/api/task-runs/{id}/logs";
      const id = taskRunId ?? runId;
      const lines: Log[] = [];
      let after = 0;
      for (let page = 0; page < MAX_PAGES; page++) {
        const { items, next_cursor } = unwrap(
          await api.GET(
            path as any,
            { params: { path: { id }, query: { after, limit: 1000, level: "", search } } } as any,
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

/** "20:27:19.031", local time with milliseconds. */
export function logTime(micros: number): string {
  const d = new Date(micros / 1000);
  const hms = d.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
  return `${hms}.${String(d.getMilliseconds()).padStart(3, "0")}`;
}

/** "05 Oct 2026", the local day a line was logged on. */
export function logDate(micros: number): string {
  return new Date(micros / 1000).toLocaleDateString(undefined, {
    day: "2-digit",
    month: "short",
    year: "numeric",
  });
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

function LevelChips({
  counts,
  value,
  onChange,
}: {
  counts: Record<string, number>;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <fieldset aria-label="Log level" className="flex flex-wrap gap-1.5">
      {LEVEL_CHIPS.filter((c) => c.value !== "DEBUG" || counts.DEBUG > 0).map((c) => (
        <button
          key={c.value}
          type="button"
          aria-pressed={c.value === value}
          onClick={() => onChange(c.value)}
          data-testid="level-chip"
          className={cn(
            "inline-flex h-7 cursor-pointer items-center gap-1.5 rounded-full border px-2.5 text-xs font-medium",
            c.value === value
              ? "border-foreground bg-foreground text-background"
              : "bg-card text-foreground hover:bg-accent",
          )}
        >
          {c.label}
          <span className="tabular-nums opacity-75">{counts[c.value] ?? 0}</span>
        </button>
      ))}
    </fieldset>
  );
}

/** Lines under a header for each local day they span, time only on each line. */
function LogTable({ lines, taskKeys }: { lines: Log[]; taskKeys?: Record<number, string> }) {
  let day = "";
  let firstError = true;
  return (
    <table className="w-full">
      <tbody>
        {lines.map((line) => {
          const level = levelName(line.level);
          const lineDay = logDate(line.timestamp);
          const header = lineDay !== day;
          day = lineDay;
          const isError = line.level >= 40;
          const first = isError && firstError;
          if (isError) firstError = false;
          const source = taskKeys
            ? line.task_run_id != null
              ? (taskKeys[line.task_run_id] ?? line.logger)
              : "run"
            : line.logger;
          return (
            <Fragment key={line.id}>
              {header ? (
                <tr data-testid="log-date">
                  <td colSpan={4} className="px-3 pt-1.5 pb-1 font-sans text-xs text-muted-foreground">
                    Times are local · {lineDay}
                  </td>
                </tr>
              ) : null}
              <tr
                data-first-error={first ? "true" : undefined}
                className={cn(
                  "align-top hover:bg-accent/40",
                  isError &&
                    "bg-destructive/5 shadow-[inset_3px_0_0_var(--destructive)] dark:bg-destructive/10",
                  level === "WARNING" && "bg-amber-500/5",
                )}
              >
                <td className="px-3 py-0.5 whitespace-nowrap text-muted-foreground tabular-nums">
                  {logTime(line.timestamp)}
                </td>
                <td className={cn("px-2 py-0.5 whitespace-nowrap", LEVEL_TEXT[level])}>{level}</td>
                <td className="px-2 py-0.5 whitespace-nowrap text-muted-foreground">{source}</td>
                <td className="w-full px-2 py-0.5 whitespace-pre-wrap">{line.message}</td>
              </tr>
            </Fragment>
          );
        })}
      </tbody>
    </table>
  );
}

/** Saves the lines as text, one per line with the full local timestamp. */
function download(lines: Log[], name: string) {
  const text = lines
    .map(
      (l) =>
        `${new Date(l.timestamp / 1000).toISOString()} ${levelName(l.level).padEnd(8)} ${l.logger} ${l.message}`,
    )
    .join("\n");
  const url = URL.createObjectURL(new Blob([`${text}\n`], { type: "text/plain" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  URL.revokeObjectURL(url);
}

/**
 * A run's log lines with level chips that count each level, a text search, a
 * download, and Follow, which keeps the newest line in view while the panel is
 * `active`. With `taskRunId` the lines are that task run's, shown as a chip
 * that `onClearTask` removes. `taskKeys` names each line's task run.
 *
 * Each change of `focusError` scrolls to the first ERROR line once it has
 * loaded, and turns Follow off so the line stays in view.
 */
export function RunLogs({
  runId,
  taskRunId,
  taskLabel,
  onClearTask,
  active,
  taskKeys,
  focusError,
}: {
  runId: number;
  taskRunId?: number;
  taskLabel?: string;
  onClearTask?: () => void;
  active?: boolean;
  taskKeys?: Record<number, string>;
  focusError?: number;
}) {
  const [level, setLevel] = useState("");
  const [search, setSearch] = useState("");
  const [follow, setFollow] = useState(true);
  const query = useLogLines(runId, taskRunId, search);
  const all = query.data ?? [];
  const counts = Object.fromEntries(
    LEVEL_CHIPS.map((c) => [c.value, all.filter((l) => c.match(l.level)).length]),
  );
  const chip = LEVEL_CHIPS.find((c) => c.value === level) ?? LEVEL_CHIPS[0];
  const lines = all.filter((l) => chip.match(l.level));
  const end = useStickToEnd(lines.length, follow && !!active);
  const list = useRef<HTMLDivElement>(null);
  // The focus request last served, so new lines do not pull the view back.
  const served = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (focusError == null || focusError === served.current) return;
    if (level !== "" && level !== "ERROR") {
      setLevel("");
      return;
    }
    const row = list.current?.querySelector<HTMLElement>("[data-first-error]");
    if (!row) return;
    served.current = focusError;
    setFollow(false);
    row.scrollIntoView({ block: "center" });
  });
  return (
    <div className="flex h-full flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <LevelChips counts={counts} value={level} onChange={setLevel} />
        <Input
          placeholder="Search logs"
          aria-label="Search logs"
          className="h-7 max-w-xs"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        {taskLabel ? <TaskChip label={taskLabel} onClear={onClearTask} /> : null}
        <span className="ml-auto flex items-center gap-3 text-xs text-muted-foreground">
          Follow
          <Switch checked={follow} onCheckedChange={setFollow} aria-label="Follow" />
          <Button
            type="button"
            variant="outline"
            size="xs"
            disabled={!lines.length}
            onClick={() =>
              download(lines, `run-${runId}${taskRunId != null ? `-task-${taskRunId}` : ""}-logs.txt`)
            }
          >
            <Download /> Download
          </Button>
        </span>
      </div>
      <div
        ref={list}
        className="min-h-64 flex-1 overflow-auto rounded-lg border bg-card py-1 font-mono text-xs"
        data-testid="log-list"
      >
        {lines.length ? (
          <LogTable lines={lines} taskKeys={taskKeys} />
        ) : (
          <div className="p-4 text-muted-foreground">
            {query.isLoading ? "Loading logs" : all.length ? "No lines at this level" : "No logs"}
          </div>
        )}
        <div ref={end} />
      </div>
    </div>
  );
}
