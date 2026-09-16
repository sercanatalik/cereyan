/**
 * The scope sidebar beside the scoped pages: every project, and under each its
 * groups. Picking one sets the scope, a project and a group within it, for the
 * whole UI.
 */
import { useQuery } from "@tanstack/react-query";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { ChevronRight, GitMerge, LayoutGrid } from "lucide-react";
import { useState } from "react";
import { api, type Flow, unwrap } from "@/api/client";
import { upstreamNames } from "@/components/flow-graph";
import { StateBar } from "@/components/state-bar";
import { nestByProject } from "@/lib/groups";
import { useProject } from "@/lib/project";
import { cn } from "@/lib/utils";

/** Counts of each flow's last-run state, for a state bar. */
export function lastStates(flows: Flow[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const f of flows) {
    const last = f.recent_runs[0];
    if (last) counts[last[1]] = (counts[last[1]] ?? 0) + 1;
  }
  return counts;
}

/**
 * The scope as the sidebar shows it, and how it changes it. A page opened
 * from a link carrying `project` or `group` shows that scope, so it is the one
 * marked; picking drops those link parameters so the choice is not overridden.
 */
function useScopeControl() {
  const { scope, group: scopeGroup, setScope } = useProject();
  const linked = useRouterState({ select: (s) => s.location.search as { project?: string; group?: string } });
  const project = linked.project ?? scope;
  const group = linked.group ?? (project === scope ? scopeGroup : "");
  const navigate = useNavigate();
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const pick = (p: string, g = "") => {
    setScope(p, g);
    navigate({
      to: ".",
      search: (old: Record<string, unknown>) => ({
        ...old,
        project: undefined,
        group: undefined,
        flow: undefined,
      }),
      replace: true,
    } as any);
  };
  return { project, group, pick, all: flows.data ?? [], tree: nestByProject(flows.data ?? []) };
}

/** The sidebar beside the scoped pages: every project, and under each its groups. */
export function ScopeSidebar() {
  const { project, group, pick: onPick, all, tree } = useScopeControl();
  const total = all.length;
  const [closed, setClosed] = useState<Record<string, boolean>>({});
  const row = "flex items-center gap-2 rounded-md text-left text-sm hover:bg-accent";
  return (
    <aside
      aria-label="Scope"
      className="flex min-h-0 flex-col overflow-auto border-r bg-card px-2.5 py-3.5"
      data-testid="scope-sidebar"
    >
      <button
        type="button"
        className={cn(row, "h-8 px-2 font-medium", !project && "bg-accent font-semibold")}
        aria-current={!project ? "true" : undefined}
        onClick={() => onPick("")}
      >
        <LayoutGrid className="size-3.5 text-muted-foreground" />
        <span className="flex-1">All projects</span>
        <span className="text-xs font-normal text-muted-foreground">{total}</span>
      </button>
      <div className="mx-2 mt-2.5 mb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
        Projects
      </div>
      {tree.map((p) => {
        const open = !closed[p.key];
        const active = project === p.key && !group;
        // A project with only its own flows is one group; listing it again adds nothing.
        const groups = [
          ...(p.rows.length && p.groups.length ? [{ key: p.key, label: "(project)", items: p.rows }] : []),
          ...p.groups.map((g) => ({ key: g.key, label: g.key, items: g.items })),
        ];
        return (
          <div key={p.key} className="flex flex-col" data-testid={`scope-project-${p.key}`}>
            <div className={cn("flex h-8 items-center rounded-md hover:bg-accent", active && "bg-accent")}>
              <button
                type="button"
                aria-label={`${open ? "Collapse" : "Expand"} ${p.key}`}
                aria-expanded={open}
                className="inline-flex h-8 w-6 shrink-0 items-center justify-center text-muted-foreground disabled:opacity-0"
                disabled={groups.length === 0}
                onClick={() => setClosed({ ...closed, [p.key]: open })}
              >
                <ChevronRight className={cn("size-3.5 transition-transform", open && "rotate-90")} />
              </button>
              <button
                type="button"
                aria-current={active ? "true" : undefined}
                className={cn(
                  "flex h-8 min-w-0 flex-1 items-center gap-2 pr-2 text-left text-sm",
                  active ? "font-semibold" : "font-medium",
                )}
                onClick={() => onPick(p.key)}
              >
                <span className="flex-1 truncate">{p.key}</span>
                <StateBar counts={lastStates(p.items)} className="w-7" height={4} />
                <span className="min-w-3.5 text-right text-xs font-normal text-muted-foreground">
                  {p.items.length}
                </span>
              </button>
            </div>
            {open
              ? groups.map((g) => {
                  const selected = project === p.key && group === g.key;
                  return (
                    <button
                      type="button"
                      key={g.key}
                      aria-current={selected ? "true" : undefined}
                      data-testid={`scope-group-${p.key}/${g.key}`}
                      className={cn(row, "ml-6 h-[30px] px-2", selected && "bg-accent font-semibold")}
                      onClick={() => onPick(p.key, g.key)}
                    >
                      <span className="flex-1 truncate">{g.label}</span>
                      {g.items.some((f) => upstreamNames(f).length) ? (
                        <GitMerge className="size-3 text-muted-foreground" aria-label="has dependencies" />
                      ) : null}
                      <span className="min-w-3.5 text-right text-xs text-muted-foreground">
                        {g.items.length}
                      </span>
                    </button>
                  );
                })
              : null}
          </div>
        );
      })}
      <div className="mt-auto px-2 pt-3 text-[11.5px] leading-4 text-muted-foreground">
        Groups come from <span className="font-mono">@flow(group=…)</span>. A project is its own group when
        none is set.
      </div>
    </aside>
  );
}
