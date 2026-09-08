// A stacked-by-state activity histogram drawn as inline SVG.
import type { Run, StateType } from "@/api/client";

const STACK_ORDER: StateType[] = [
  "Completed",
  "Failed",
  "Crashed",
  "Cancelled",
  "Running",
  "Pending",
  "Scheduled",
  "Paused",
  "Cancelling",
];
const FILL: Record<StateType, string> = {
  Completed: "#10b981",
  Failed: "#ef4444",
  Crashed: "#f97316",
  Cancelled: "#a1a1aa",
  Running: "#0ea5e9",
  Pending: "#94a3b8",
  Scheduled: "#f59e0b",
  Paused: "#8b5cf6",
  Cancelling: "#d4d4d8",
};

function tickLabel(micros: number, spanMicros: number): string {
  const d = new Date(micros / 1000);
  if (spanMicros > 2 * 86_400_000_000) {
    return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  }
  return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

export function Histogram({
  runs,
  start,
  end,
  buckets = 24,
}: {
  runs: Run[];
  start: number;
  end: number;
  buckets?: number;
}) {
  const width = 1200;
  const height = 120;
  const top = 6;
  const bottom = 20;
  const gap = 6;
  const plot = height - top - bottom;
  const span = Math.max(end - start, 1);
  const bins = Array.from({ length: buckets }, (_, i) => ({
    from: start + (i * span) / buckets,
    counts: {} as Record<StateType, number>,
  }));
  for (const run of runs) {
    const t = run.start_time ?? run.created_at;
    const index = Math.min(buckets - 1, Math.max(0, Math.floor(((t - start) / span) * buckets)));
    bins[index].counts[run.state.type] = (bins[index].counts[run.state.type] ?? 0) + 1;
  }
  const max = Math.max(1, ...bins.map((b) => Object.values(b.counts).reduce((a, c) => a + c, 0)));
  const barWidth = (width - gap * (buckets - 1)) / buckets;
  const tickEvery = Math.max(1, Math.round(buckets / 6));
  const baseline = top + plot + 0.5;
  return (
    <svg viewBox={`0 0 ${width} ${height}`} className="h-[120px] w-full" role="img" aria-label="Run activity">
      {bins.map((bin, i) => {
        let y = top + plot;
        const x = i * (barWidth + gap);
        return (
          <g key={bin.from}>
            {STACK_ORDER.map((state) => {
              const n = bin.counts[state] ?? 0;
              if (!n) return null;
              const h = (n / max) * plot;
              y -= h;
              return (
                <rect
                  key={state}
                  x={x}
                  y={y}
                  width={barWidth}
                  height={Math.max(h - 1, 1)}
                  fill={FILL[state]}
                  rx={2}
                />
              );
            })}
          </g>
        );
      })}
      <line x1={0} x2={width} y1={baseline} y2={baseline} className="stroke-border" />
      {bins.map((bin, i) =>
        i % tickEvery === 0 ? (
          <text
            key={`t${bin.from}`}
            x={i * (barWidth + gap)}
            y={height - 4}
            fontSize={11}
            className="fill-muted-foreground font-mono"
          >
            {tickLabel(bin.from, span)}
          </text>
        ) : null,
      )}
      <line
        x1={(buckets - 1) * (barWidth + gap) + barWidth / 2}
        x2={(buckets - 1) * (barWidth + gap) + barWidth / 2}
        y1={top}
        y2={top + plot}
        strokeDasharray="2 3"
        className="stroke-muted-foreground"
        data-testid="now-marker"
      />
    </svg>
  );
}
