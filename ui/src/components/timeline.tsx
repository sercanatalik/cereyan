// SVG timeline of a run's task runs: temporal (Gantt) or dependency layout,
// selection panel, fullscreen, and row virtualization above 500 nodes.
import { Link } from "@tanstack/react-router";
import { Maximize2, Minimize2 } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import type { components } from "@/api/schema";
import { StateBadge } from "@/components/ported/state-badge";
import { Button } from "@/components/ui/button";
import { cn, formatDuration, formatTime } from "@/lib/utils";

export type Graph = components["schemas"]["RunGraph"];
type Node = Graph["nodes"][number];

const FILL: Record<string, string> = {
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

const ROW = 22;
const VIRTUAL_ABOVE = 500;

export function dependencyLevels(nodes: Node[], edges: Graph["edges"]): Map<number, number> {
  const parents = new Map<number, number[]>();
  for (const e of edges) parents.set(e.to, [...(parents.get(e.to) ?? []), e.from]);
  const level = new Map<number, number>();
  const visit = (id: number, depth = 0): number => {
    if (level.has(id)) return level.get(id) as number;
    if (depth > nodes.length) return 0;
    const ps = parents.get(id) ?? [];
    const l = ps.length ? Math.max(...ps.map((p) => visit(p, depth + 1))) + 1 : 0;
    level.set(id, l);
    return l;
  };
  for (const n of nodes) visit(n.id);
  return level;
}

export function Timeline({ graph, now }: { graph: Graph; now?: number }) {
  const [layout, setLayout] = useState<"temporal" | "dependency">("temporal");
  const [selected, setSelected] = useState<Node | null>(null);
  const [full, setFull] = useState(false);
  const [scrollTop, setScrollTop] = useState(0);
  const container = useRef<HTMLDivElement>(null);
  const nodes = useMemo(
    () => graph.nodes.slice().sort((a, b) => (a.start_time ?? a.created_at) - (b.start_time ?? b.created_at)),
    [graph],
  );
  const levels = useMemo(() => dependencyLevels(nodes, graph.edges), [nodes, graph.edges]);
  const current = now ?? Date.now() * 1000;
  const minT = Math.min(...nodes.map((n) => n.start_time ?? n.created_at), current);
  const maxT = Math.max(...nodes.map((n) => n.end_time ?? current), minT + 1);
  const width = 900;
  const labelWidth = 180;
  const span = Math.max(maxT - minT, 1);
  const x = (t: number) => labelWidth + ((t - minT) / span) * (width - labelWidth - 12);
  const maxLevel = Math.max(0, ...Array.from(levels.values()));
  const colWidth = (width - labelWidth - 12) / (maxLevel + 1);
  const height = nodes.length * ROW + 8;
  const virtual = nodes.length > VIRTUAL_ABOVE;
  const viewport = 480;
  const first = virtual ? Math.max(0, Math.floor(scrollTop / ROW) - 5) : 0;
  const last = virtual ? Math.min(nodes.length, Math.ceil((scrollTop + viewport) / ROW) + 5) : nodes.length;
  const index = new Map(nodes.map((n, i) => [n.id, i]));
  const bar = (n: Node) => {
    const i = index.get(n.id) ?? 0;
    const y = i * ROW + 4;
    if (layout === "temporal") {
      const s = n.start_time ?? n.created_at;
      const e = n.end_time ?? current;
      const x0 = x(s);
      const w = Math.max(3, x(e) - x0);
      return { x: x0, y, w, h: ROW - 8 };
    }
    const l = levels.get(n.id) ?? 0;
    return { x: labelWidth + l * colWidth + 4, y, w: Math.max(30, colWidth - 8), h: ROW - 8 };
  };
  const visible = nodes.slice(first, last);
  return (
    <div className={cn("flex gap-3", full && "fixed inset-0 z-40 bg-background p-4")} data-testid="timeline">
      <div className="min-w-0 flex-1 rounded-md border bg-card">
        <div className="flex items-center justify-between border-b px-3 py-1.5 text-xs">
          <div className="flex gap-1">
            {(["temporal", "dependency"] as const).map((l) => (
              <button
                key={l}
                type="button"
                className={cn(
                  "rounded px-2 py-0.5",
                  layout === l ? "bg-accent font-medium" : "text-muted-foreground",
                )}
                onClick={() => setLayout(l)}
              >
                {l}
              </button>
            ))}
          </div>
          <span className="text-muted-foreground">
            {nodes.length} task runs{virtual ? " (virtualized)" : ""}
          </span>
          <Button variant="ghost" size="icon" aria-label="Fullscreen" onClick={() => setFull((f) => !f)}>
            {full ? <Minimize2 className="h-4 w-4" /> : <Maximize2 className="h-4 w-4" />}
          </Button>
        </div>
        <div
          ref={container}
          className="overflow-auto"
          style={{ maxHeight: full ? "calc(100vh - 80px)" : viewport }}
          onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
        >
          <svg
            viewBox={`0 0 ${width} ${height}`}
            width="100%"
            style={{ height }}
            role="img"
            aria-label="Task run timeline"
          >
            {graph.edges.map((e) => {
              const from = nodes[index.get(e.from) ?? -1];
              const to = nodes[index.get(e.to) ?? -1];
              if (!from || !to) return null;
              const a = bar(from);
              const b = bar(to);
              return (
                <path
                  key={`${e.from}-${e.to}`}
                  d={`M ${a.x + a.w} ${a.y + a.h / 2} C ${a.x + a.w + 20} ${a.y + a.h / 2}, ${b.x - 20} ${b.y + b.h / 2}, ${b.x} ${b.y + b.h / 2}`}
                  fill="none"
                  stroke="currentColor"
                  strokeOpacity={0.35}
                  strokeWidth={1.2}
                />
              );
            })}
            {visible.map((n) => {
              const b = bar(n);
              return (
                <g
                  key={n.id}
                  onClick={() => setSelected(n)}
                  className="cursor-pointer"
                  data-node={n.dynamic_key}
                >
                  <text x={6} y={b.y + b.h - 3} fontSize={11} fill="currentColor" opacity={0.8}>
                    {n.dynamic_key.length > 26 ? `${n.dynamic_key.slice(0, 25)}…` : n.dynamic_key}
                  </text>
                  <rect
                    x={b.x}
                    y={b.y}
                    width={b.w}
                    height={b.h}
                    rx={3}
                    fill={FILL[n.state.type] ?? "#94a3b8"}
                    stroke={selected?.id === n.id ? "currentColor" : "none"}
                    strokeWidth={1.5}
                  />
                </g>
              );
            })}
          </svg>
        </div>
      </div>
      {selected ? (
        <aside
          className="w-64 shrink-0 space-y-2 rounded-md border bg-card p-3 text-sm"
          data-testid="timeline-panel"
        >
          <div className="font-medium">{selected.dynamic_key}</div>
          <StateBadge state={selected.state} />
          <div className="text-xs text-muted-foreground">Start {formatTime(selected.start_time)}</div>
          <div className="text-xs text-muted-foreground">End {formatTime(selected.end_time)}</div>
          <div className="text-xs text-muted-foreground">
            Duration{" "}
            {selected.start_time && selected.end_time
              ? formatDuration(selected.end_time - selected.start_time)
              : "-"}
          </div>
          <Link
            to="/task-runs/$taskRunId"
            params={{ taskRunId: String(selected.id) }}
            className="text-xs underline"
          >
            Open task run
          </Link>
        </aside>
      ) : null}
    </div>
  );
}
