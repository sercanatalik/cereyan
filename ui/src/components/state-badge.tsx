import type { RunState, StateType } from "@/api/client";
import { cn } from "@/lib/utils";

/** How a state type looks: the pill's background and text, and its dot. */
const TYPE_STYLE: Record<StateType, { pill: string; dot: string }> = {
  Scheduled: {
    pill: "bg-amber-100 text-amber-900 dark:bg-amber-900/40 dark:text-amber-200",
    dot: "bg-amber-400",
  },
  Pending: { pill: "bg-slate-200 text-slate-800 dark:bg-slate-700 dark:text-slate-100", dot: "bg-slate-400" },
  Running: { pill: "bg-sky-100 text-sky-900 dark:bg-sky-900/40 dark:text-sky-200", dot: "bg-sky-500" },
  Completed: {
    pill: "bg-emerald-100 text-emerald-900 dark:bg-emerald-900/40 dark:text-emerald-200",
    dot: "bg-emerald-500",
  },
  Failed: { pill: "bg-red-100 text-red-900 dark:bg-red-900/40 dark:text-red-200", dot: "bg-red-500" },
  Cancelled: { pill: "bg-zinc-200 text-zinc-800 dark:bg-zinc-700 dark:text-zinc-100", dot: "bg-zinc-400" },
  Crashed: {
    pill: "bg-orange-100 text-orange-900 dark:bg-orange-900/40 dark:text-orange-200",
    dot: "bg-orange-500",
  },
  Paused: {
    pill: "bg-violet-100 text-violet-900 dark:bg-violet-900/40 dark:text-violet-200",
    dot: "bg-violet-500",
  },
  Cancelling: { pill: "bg-zinc-100 text-zinc-700 dark:bg-zinc-800 dark:text-zinc-200", dot: "bg-zinc-300" },
};

/** Sub-states that read differently from their type; a missing `dot` keeps the type's. */
const SUB_STATE_STYLE: Record<string, { pill: string; dot?: string }> = {
  Skipped: { pill: "bg-teal-100 text-teal-900 dark:bg-teal-900/40 dark:text-teal-200", dot: "bg-teal-500" },
  Cached: { pill: "bg-cyan-100 text-cyan-900 dark:bg-cyan-900/40 dark:text-cyan-200", dot: "bg-cyan-500" },
  Late: {
    pill: "bg-orange-100 text-orange-900 dark:bg-orange-900/40 dark:text-orange-200",
    dot: "bg-orange-500",
  },
  AwaitingResource: { pill: "bg-amber-50 text-amber-900 dark:bg-amber-900/30 dark:text-amber-200" },
  AwaitingRetry: { pill: "bg-amber-100 text-amber-900 dark:bg-amber-900/40 dark:text-amber-200" },
  TimedOut: { pill: "bg-red-100 text-red-900 dark:bg-red-900/40 dark:text-red-200" },
};

/** The dot colour of each state type, shared by bars, feeds, and rollups. */
export const DOT_COLORS = Object.fromEntries(
  Object.entries(TYPE_STYLE).map(([type, style]) => [type, style.dot]),
) as Record<StateType, string>;

/** A run or task run state as a pill: a dot and the state's name. */
export function StateBadge({ state, className }: { state: RunState | null | undefined; className?: string }) {
  if (!state) return <span className="text-muted-foreground">-</span>;
  const sub = SUB_STATE_STYLE[state.name];
  const base = TYPE_STYLE[state.type];
  const waitingOn =
    state.name === "AwaitingResource" ? (state.details as { resource?: string })?.resource : undefined;
  return (
    <span
      data-state={state.type}
      data-name={state.name}
      title={waitingOn ? `waiting for ${waitingOn}` : (state.message ?? undefined)}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium",
        sub?.pill ?? base.pill,
        className,
      )}
    >
      <span
        className={cn(
          "h-1.5 w-1.5 rounded-full",
          sub?.dot ?? base.dot,
          state.type === "Running" && "animate-pulse",
        )}
      />
      {state.name}
    </span>
  );
}

/** Just the dot, for places too tight for a pill. */
export function StateDot({ type, title }: { type: StateType; title?: string }) {
  return <span title={title} className={cn("inline-block h-2.5 w-2.5 rounded-full", DOT_COLORS[type])} />;
}
