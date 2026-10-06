import { Link } from "@tanstack/react-router";
import type { Flow, StateType } from "@/api/client";
import { DOT_COLORS } from "@/components/state-badge";
import { cn, formatDuration } from "@/lib/utils";

/** One of a flow's recent runs: id, state type, state name, duration in µs. */
export type RecentRun = Flow["recent_runs"][number];

const SLOTS = 10;

function describe([id, , name, duration]: RecentRun): string {
  return `Run ${id} · ${name}${duration == null ? "" : ` · ${formatDuration(duration)}`}`;
}

/**
 * A flow's last ten runs as equal squares, newest on the right, with a neutral
 * square for each slot that has no run. Each square links to its run, or, given
 * `onSelect`, is a button that picks it; `selectedId` marks the picked one.
 */
export function RunStrip({
  runs,
  onSelect,
  selectedId,
  className,
}: {
  runs: RecentRun[];
  onSelect?: (run: RecentRun) => void;
  selectedId?: number | null;
  className?: string;
}) {
  const ordered = runs.slice(0, SLOTS).reverse();
  const empty = SLOTS - ordered.length;
  const square = "block h-5 w-3 shrink-0 rounded-[3px]";
  return (
    <span className={cn("inline-flex items-center gap-[3px]", className)} data-testid="run-strip">
      {Array.from({ length: empty }, (_, i) => (
        // biome-ignore lint/suspicious/noArrayIndexKey: empty slots have no identity
        <span key={`empty-${i}`} className={cn(square, "bg-muted")} data-state="none" aria-hidden />
      ))}
      {ordered.map((run) => {
        const [id, type] = run;
        const label = describe(run);
        const colour = DOT_COLORS[type as StateType] ?? "bg-muted-foreground";
        if (onSelect) {
          const selected = selectedId === id;
          return (
            <button
              key={id}
              type="button"
              title={label}
              aria-label={`${label}: show logs`}
              aria-pressed={selected}
              data-state={type}
              onClick={() => onSelect(run)}
              className={cn(
                square,
                colour,
                "cursor-pointer outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1",
                selected && "ring-2 ring-foreground ring-offset-2 ring-offset-card",
              )}
            />
          );
        }
        return (
          <Link
            key={id}
            to="/runs/$runId"
            params={{ runId: String(id) }}
            title={label}
            aria-label={label}
            data-state={type}
            className={cn(square, colour)}
          />
        );
      })}
    </span>
  );
}

/** The colours a run strip uses, named once above a list of strips. */
export function RunStripLegend({ className }: { className?: string }) {
  const items: [StateType, string][] = [
    ["Completed", "Completed"],
    ["Failed", "Failed"],
    ["Crashed", "Crashed"],
    ["Paused", "Waiting"],
    ["Scheduled", "Scheduled"],
  ];
  return (
    <span className={cn("flex flex-wrap items-center gap-3 text-xs text-muted-foreground", className)}>
      {items.map(([type, label]) => (
        <span key={type} className="inline-flex items-center gap-1.5">
          <span className={cn("size-2 rounded-[2px]", DOT_COLORS[type])} />
          {label}
        </span>
      ))}
    </span>
  );
}
