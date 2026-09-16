// Flow dependencies: which flows start after which, from `after=` chains and fan-ins.
import type { Flow } from "@/api/client";

/** The flows this one starts after: its fan-in upstreams, else the one it follows. */
export function upstreamNames(f: Flow): string[] {
  return f.upstreams?.length ? f.upstreams : f.triggered_by ? [f.triggered_by] : [];
}

export function dependencyEdges(flows: Flow[]): { from: Flow; to: Flow }[] {
  const edges: { from: Flow; to: Flow }[] = [];
  for (const f of flows) {
    for (const name of upstreamNames(f)) {
      const up = flows.find((u) => u.project === f.project && u.name === name);
      if (up) edges.push({ from: up, to: f });
    }
  }
  return edges;
}
