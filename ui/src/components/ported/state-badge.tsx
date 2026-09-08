// Ported from Prefect's StateBadge (Apache 2.0); see NOTICE.

import type { RunState, StateType } from "@/api/client";
import { cn } from "@/lib/utils";

const COLORS: Record<StateType, string> = {
  Scheduled: "bg-amber-100 text-amber-900 dark:bg-amber-900/40 dark:text-amber-200",
  Pending: "bg-slate-200 text-slate-800 dark:bg-slate-700 dark:text-slate-100",
  Running: "bg-sky-100 text-sky-900 dark:bg-sky-900/40 dark:text-sky-200",
  Completed: "bg-emerald-100 text-emerald-900 dark:bg-emerald-900/40 dark:text-emerald-200",
  Failed: "bg-red-100 text-red-900 dark:bg-red-900/40 dark:text-red-200",
  Cancelled: "bg-zinc-200 text-zinc-800 dark:bg-zinc-700 dark:text-zinc-100",
  Crashed: "bg-orange-100 text-orange-900 dark:bg-orange-900/40 dark:text-orange-200",
  Paused: "bg-violet-100 text-violet-900 dark:bg-violet-900/40 dark:text-violet-200",
  Cancelling: "bg-zinc-100 text-zinc-700 dark:bg-zinc-800 dark:text-zinc-200",
};

export const DOT_COLORS: Record<StateType, string> = {
  Scheduled: "bg-amber-400",
  Pending: "bg-slate-400",
  Running: "bg-sky-500",
  Completed: "bg-emerald-500",
  Failed: "bg-red-500",
  Cancelled: "bg-zinc-400",
  Crashed: "bg-orange-500",
  Paused: "bg-violet-500",
  Cancelling: "bg-zinc-300",
};

const NAME_COLORS: Record<string, string> = {
  Skipped: "bg-teal-100 text-teal-900 dark:bg-teal-900/40 dark:text-teal-200",
  Cached: "bg-cyan-100 text-cyan-900 dark:bg-cyan-900/40 dark:text-cyan-200",
  AwaitingResource: "bg-amber-50 text-amber-900 dark:bg-amber-900/30 dark:text-amber-200",
  Late: "bg-orange-100 text-orange-900 dark:bg-orange-900/40 dark:text-orange-200",
  AwaitingRetry: "bg-amber-100 text-amber-900 dark:bg-amber-900/40 dark:text-amber-200",
  TimedOut: "bg-red-100 text-red-900 dark:bg-red-900/40 dark:text-red-200",
};

const NAME_DOTS: Record<string, string> = {
  Skipped: "bg-teal-500",
  Cached: "bg-cyan-500",
  Late: "bg-orange-500",
};

export function StateBadge({ state, className }: { state: RunState | null | undefined; className?: string }) {
  if (!state) return <span className="text-muted-foreground">-</span>;
  const resource = (state.details as any)?.resource;
  const title =
    state.name === "AwaitingResource" && resource ? `waiting for ${resource}` : (state.message ?? undefined);
  return (
    <span
      data-state={state.type}
      data-name={state.name}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium",
        NAME_COLORS[state.name] ?? COLORS[state.type],
        className,
      )}
      title={title}
    >
      <span
        className={cn(
          "h-1.5 w-1.5 rounded-full",
          NAME_DOTS[state.name] ?? DOT_COLORS[state.type],
          state.type === "Running" && "animate-pulse",
        )}
      />
      {state.name}
    </span>
  );
}

export function StateDot({ type, title }: { type: StateType; title?: string }) {
  return <span title={title} className={cn("inline-block h-2.5 w-2.5 rounded-full", DOT_COLORS[type])} />;
}
