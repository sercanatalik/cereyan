import { useEffect, useState } from "react";
import type { TaskRun } from "@/api/client";
import { StateDot } from "@/components/ported/state-badge";
import { StateBar, type StateCounts } from "@/components/state-bar";
import { cn, formatDuration } from "@/lib/utils";

const DONE = new Set(["Completed", "Failed", "Crashed", "Cancelled"]);

function useNow(active: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [active]);
  return now;
}

/** "retry 2 of 3 · in 24s" for a task waiting on its next attempt. */
export function retryLine(task: TaskRun, nowMs: number): string | null {
  if (task.state.name !== "AwaitingRetry") return null;
  const d = (task.state.details ?? {}) as { attempt?: number; delay?: number; retries?: number };
  const due = task.state.timestamp / 1000 + (d.delay ?? 0) * 1000;
  const left = Math.max(0, Math.round((due - nowMs) / 1000));
  const which =
    d.retries != null ? `retry ${d.attempt ?? "?"} of ${d.retries}` : `retry ${d.attempt ?? ""}`.trim();
  return `${which} · ${left > 0 ? `in ${left}s` : "now"}`;
}

export function taskCounts(tasks: TaskRun[]): StateCounts {
  const counts: Record<string, number> = {};
  for (const t of tasks) {
    const k = t.state.name === "AwaitingRetry" ? "AwaitingRetry" : t.state.type;
    counts[k] = (counts[k] ?? 0) + 1;
  }
  return counts;
}

/** The left pane of the run page: every task run, with the one in focus highlighted. */
export function TaskRail({
  tasks,
  selectedId,
  onSelect,
  className,
}: {
  tasks: TaskRun[];
  selectedId?: number;
  onSelect: (id: number | undefined) => void;
  className?: string;
}) {
  const now = useNow(tasks.some((t) => t.state.name === "AwaitingRetry" || t.state.type === "Running"));
  const done = tasks.filter((t) => DONE.has(t.state.type)).length;
  return (
    <aside className={cn("flex flex-col border-r", className)} data-testid="task-rail">
      <div className="flex flex-col gap-2 border-b px-4 pt-3.5 pb-2.5">
        <div className="flex items-center justify-between">
          <span className="font-semibold">Tasks</span>
          <span className="text-xs tabular-nums text-muted-foreground">
            {done} of {tasks.length} done
          </span>
        </div>
        <StateBar counts={taskCounts(tasks)} className="w-full" height={6} />
      </div>
      <div className="flex flex-col p-1.5" role="listbox" aria-label="Task runs">
        {tasks.map((t) => {
          const selected = t.id === selectedId;
          const sub = retryLine(t, now);
          const duration =
            t.total_run_time != null
              ? formatDuration(t.total_run_time)
              : t.state.type === "Running" && t.start_time
                ? formatDuration(now * 1000 - t.start_time)
                : "";
          return (
            <button
              key={t.id}
              type="button"
              role="option"
              aria-selected={selected}
              data-task-id={t.id}
              onClick={() => onSelect(selected ? undefined : t.id)}
              className={cn(
                "grid grid-cols-[14px_minmax(0,1fr)_auto] items-center gap-x-2.5 rounded-md px-2 py-1.5 text-left hover:bg-accent/60",
                selected && "bg-accent",
              )}
            >
              <StateDot type={t.state.type} title={t.state.name} />
              <span className="flex min-w-0 flex-col">
                <span className="truncate font-medium">{t.dynamic_key}</span>
                {sub ? (
                  <span
                    className="text-[11.5px] leading-[15px] text-muted-foreground"
                    data-testid="retry-line"
                  >
                    {sub}
                  </span>
                ) : null}
              </span>
              <span className="text-xs tabular-nums text-muted-foreground">{duration}</span>
            </button>
          );
        })}
        {tasks.length === 0 ? <div className="px-2 py-3 text-muted-foreground">No task runs yet.</div> : null}
      </div>
    </aside>
  );
}
