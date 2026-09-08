// Ported from Prefect's RunLogs viewer (Apache 2.0); see NOTICE.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { api, type Log, unwrap } from "@/api/client";
import { Input, Select } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { useLiveEvent } from "@/lib/live";
import { cn, formatTime, levelName } from "@/lib/utils";

const LEVEL_CLASS: Record<string, string> = {
  DEBUG: "text-muted-foreground",
  INFO: "text-sky-700 dark:text-sky-300",
  WARNING: "text-amber-700 dark:text-amber-300",
  ERROR: "text-red-700 dark:text-red-300",
  CRITICAL: "text-red-800 font-semibold dark:text-red-200",
};

export function RunLogs({
  runId,
  taskRunId,
  taskLabel,
  onClearTask,
  active,
}: {
  runId: number;
  taskRunId?: number;
  /** Shown as a clearable chip when the stream is filtered to one task run. */
  taskLabel?: string;
  onClearTask?: () => void;
  active?: boolean;
}) {
  const [level, setLevel] = useState("");
  const [search, setSearch] = useState("");
  const [follow, setFollow] = useState(true);
  const client = useQueryClient();
  const bottom = useRef<HTMLDivElement>(null);
  const key = ["logs", runId, taskRunId ?? null, level, search];
  const query = useQuery({
    queryKey: key,
    queryFn: async () => {
      const path = taskRunId ? "/api/task-runs/{id}/logs" : "/api/runs/{id}/logs";
      const items: Log[] = [];
      let after = 0;
      for (let page = 0; page < 20; page++) {
        const result = unwrap(
          await api.GET(
            path as any,
            {
              params: { path: { id: taskRunId ?? runId }, query: { after, level, search, limit: 1000 } },
            } as any,
          ),
        ) as { items: Log[]; next_cursor?: number | null };
        items.push(...result.items);
        if (!result.next_cursor) break;
        after = result.next_cursor;
      }
      return items;
    },
  });
  useLiveEvent("log.appended", (data) => {
    if (data?.run_id === runId) client.invalidateQueries({ queryKey: ["logs", runId] });
  });
  const count = query.data?.length ?? 0;
  useEffect(() => {
    if (follow && active && count > 0 && bottom.current) bottom.current.scrollIntoView({ block: "end" });
  }, [count, follow, active]);

  const logs = query.data ?? [];
  return (
    <div className="flex h-full flex-col gap-2">
      <div className="flex items-center gap-2">
        <Select
          value={level}
          onChange={(e) => setLevel(e.target.value)}
          aria-label="Minimum level"
          className="w-40"
        >
          <option value="">All levels</option>
          <option value="INFO">Info and above</option>
          <option value="WARNING">Warning and above</option>
          <option value="ERROR">Error and above</option>
        </Select>
        <Input
          placeholder="Search logs"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="max-w-xs"
        />
        {taskLabel ? (
          <span
            className="inline-flex h-6 items-center gap-1.5 rounded-[5px] border bg-muted pl-2 pr-1 text-xs font-medium"
            data-testid="log-task-chip"
          >
            <span className="font-normal text-muted-foreground">task</span>
            {taskLabel}
            {onClearTask ? (
              <button
                type="button"
                aria-label="Show all tasks"
                onClick={onClearTask}
                className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                <X className="size-3" />
              </button>
            ) : null}
          </span>
        ) : null}
        <span className="ml-auto flex items-center gap-2 text-xs text-muted-foreground">
          Follow
          <Switch checked={follow} onCheckedChange={setFollow} aria-label="Follow" />
        </span>
      </div>
      <div
        className="min-h-64 flex-1 overflow-auto rounded-lg border bg-card py-1 font-mono text-xs"
        data-testid="log-list"
      >
        {logs.length === 0 ? (
          <div className="p-4 text-muted-foreground">{query.isLoading ? "Loading logs" : "No logs"}</div>
        ) : (
          <table className="w-full">
            <tbody>
              {logs.map((log) => (
                <tr
                  key={log.id}
                  className={cn(
                    "align-top hover:bg-accent/40",
                    log.level >= 40 && "bg-destructive/5 dark:bg-destructive/10",
                  )}
                >
                  <td className="whitespace-nowrap px-3 py-0.5 text-muted-foreground">
                    {formatTime(log.timestamp)}
                  </td>
                  <td className={cn("whitespace-nowrap px-2 py-0.5", LEVEL_CLASS[levelName(log.level)])}>
                    {levelName(log.level)}
                  </td>
                  <td className="px-2 py-0.5 text-muted-foreground">{log.logger}</td>
                  <td className="w-full whitespace-pre-wrap px-2 py-0.5">{log.message}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <div ref={bottom} />
      </div>
    </div>
  );
}
