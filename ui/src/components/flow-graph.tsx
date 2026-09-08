// Flow dependencies: after= chains and fan-in groups, drawn as rows of nodes.
import { Link } from "@tanstack/react-router";
import type { Flow, StateType } from "@/api/client";
import { StateDot } from "@/components/ported/state-badge";

export function dependencyEdges(flows: Flow[]): { from: Flow; to: Flow }[] {
  const edges: { from: Flow; to: Flow }[] = [];
  for (const f of flows) {
    const names = f.upstreams?.length ? f.upstreams : f.triggered_by ? [f.triggered_by] : [];
    for (const name of names) {
      const up = flows.find((u) => u.project === f.project && u.name === name);
      if (up) edges.push({ from: up, to: f });
    }
  }
  return edges;
}

export interface DependencyRow {
  kind: "chain" | "fan-in";
  /** Chain: the flows in order. Fan-in: the upstreams. */
  flows: Flow[];
  /** Fan-in only: the downstream flow and its batch key. */
  into?: Flow;
  key?: string | null;
}

/** Rows for the panel: one per fan-in target, one per root-to-leaf chain. */
export function dependencyRows(flows: Flow[]): DependencyRow[] {
  const edges = dependencyEdges(flows);
  const rows: DependencyRow[] = [];
  const fanIn = new Set<number>();
  for (const f of flows) {
    const ups = edges.filter((e) => e.to.id === f.id);
    if (ups.length > 1) {
      fanIn.add(f.id);
      rows.push({ kind: "fan-in", flows: ups.map((e) => e.from), into: f, key: f.batch_key });
    }
  }
  const single = edges.filter((e) => !fanIn.has(e.to.id));
  const hasUp = new Set(single.map((e) => e.to.id));
  const roots = Array.from(new Set(single.map((e) => e.from))).filter((f) => !hasUp.has(f.id));
  const walk = (path: Flow[], depth: number) => {
    const last = path[path.length - 1];
    const next = single.filter((e) => e.from.id === last.id);
    if (next.length === 0 || depth > 50) {
      if (path.length > 1) rows.push({ kind: "chain", flows: path });
      return;
    }
    for (const e of next) walk([...path, e.to], depth + 1);
  };
  for (const root of roots) walk([root], 0);
  return rows;
}

function Arrow() {
  return (
    <svg
      width="36"
      height="12"
      viewBox="0 0 36 12"
      className="shrink-0 text-muted-foreground"
      aria-hidden="true"
    >
      <path d="M1 6h32" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path
        d="m28 1 5 5-5 5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function Node({ flow }: { flow: Flow }) {
  const last = flow.recent_runs[0];
  return (
    <Link
      to="/flows/$flowId"
      params={{ flowId: String(flow.id) }}
      className="inline-flex h-[30px] items-center gap-2 rounded-md border bg-card px-3 text-xs font-medium hover:bg-accent hover:no-underline"
      data-testid={`dep-node-${flow.name}`}
    >
      {last ? <StateDot type={last[1] as StateType} title={last[2]} /> : null}
      <span className="text-muted-foreground">{flow.project}/</span>
      {flow.name}
    </Link>
  );
}

/** The Dependencies panel on the flows page; renders nothing without `after=`. */
export function FlowGraph({ flows }: { flows: Flow[] }) {
  const rows = dependencyRows(flows);
  if (rows.length === 0) return null;
  return (
    <div className="rounded-lg border bg-card shadow-xs" data-testid="flow-graph">
      <div className="flex items-center justify-between border-b px-4 py-3">
        <span className="font-semibold">Dependencies</span>
        <span className="text-xs text-muted-foreground">flows that start after another completes</span>
      </div>
      <div className="flex flex-col gap-2.5 px-4 py-3.5">
        {rows.map((row) =>
          row.kind === "chain" ? (
            <div
              key={`chain-${row.flows.map((f) => f.id).join("-")}`}
              className="flex flex-wrap items-center gap-1.5"
            >
              {row.flows.map((f, i) => (
                <span key={f.id} className="flex items-center gap-1.5">
                  {i > 0 ? <Arrow /> : null}
                  <Node flow={f} />
                </span>
              ))}
            </div>
          ) : (
            <div
              key={`fan-${row.into?.id}`}
              className="flex flex-wrap items-center gap-1.5"
              data-testid="fan-in"
            >
              {row.flows.map((f, i) => (
                <span key={f.id} className="flex items-center gap-1.5">
                  {i > 0 ? <span className="text-xs text-muted-foreground">+</span> : null}
                  <Node flow={f} />
                </span>
              ))}
              <Arrow />
              {row.into ? <Node flow={row.into} /> : null}
              <span className="ml-2 font-mono text-[11.5px] text-muted-foreground">
                fan-in{row.key ? `, key=${row.key}` : ""}
              </span>
            </div>
          ),
        )}
      </div>
    </div>
  );
}
