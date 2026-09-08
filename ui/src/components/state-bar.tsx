import type { StateType } from "@/api/client";
import { DOT_COLORS } from "@/components/ported/state-badge";
import { cn } from "@/lib/utils";

/** Draw order: terminal states first, then what is still moving. */
export const BAR_ORDER: StateType[] = [
  "Completed",
  "Failed",
  "Crashed",
  "Cancelled",
  "Cancelling",
  "Running",
  "Paused",
  "Pending",
  "Scheduled",
];

const NAMED_COLORS: Record<string, string> = {
  Skipped: "bg-teal-500",
  Cached: "bg-cyan-500",
  Late: "bg-orange-500",
  AwaitingRetry: "bg-amber-400",
};

export type StateCounts = Partial<Record<string, number | null | undefined>>;

/**
 * A segmented bar of counts by state, for a run's task mix or the range's runs.
 * Keys may be state types or named states (Skipped, Cached, Late, AwaitingRetry).
 */
export function StateBar({
  counts,
  className,
  height = 8,
  order = BAR_ORDER,
  title,
}: {
  counts: StateCounts;
  className?: string;
  height?: number;
  order?: string[];
  title?: string;
}) {
  const keys = [...order, ...Object.keys(counts).filter((k) => !order.includes(k))];
  const parts = keys.map((k) => [k, counts[k] ?? 0] as const).filter(([, n]) => n > 0);
  const total = parts.reduce((a, [, n]) => a + n, 0);
  return (
    <span
      className={cn("inline-flex overflow-hidden rounded-[3px] bg-muted", className)}
      style={{ height, gap: 1 }}
      role="img"
      aria-label={title ?? parts.map(([k, n]) => `${n} ${k}`).join(", ")}
      title={title ?? parts.map(([k, n]) => `${n} ${k}`).join(", ")}
      data-testid="state-bar"
    >
      {parts.map(([k, n]) => (
        <span
          key={k}
          data-state={k}
          className={cn(
            "block h-full",
            NAMED_COLORS[k] ?? DOT_COLORS[k as StateType] ?? "bg-muted-foreground",
          )}
          style={{ flex: `${n} ${n} 0%`, minWidth: total > 0 ? 2 : 0 }}
        />
      ))}
    </span>
  );
}

/** "7 of 12 tasks" plus the bar, for a run in progress. */
export function TaskProgress({ counts, className }: { counts: StateCounts; className?: string }) {
  const total = Object.values(counts).reduce<number>((a, n) => a + (n ?? 0), 0);
  const done = ["Completed", "Failed", "Crashed", "Cancelled", "Skipped", "Cached"].reduce(
    (a, k) => a + (counts[k] ?? 0),
    0,
  );
  return (
    <span className={cn("flex items-center gap-2.5", className)} data-testid="task-progress">
      <StateBar counts={counts} className="flex-1" height={6} />
      <span className="whitespace-nowrap text-xs tabular-nums text-muted-foreground">
        {done} of {total} tasks
      </span>
    </span>
  );
}
