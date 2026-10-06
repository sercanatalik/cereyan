/**
 * The project scope: every project, and under each its groups. Picking one sets
 * the scope, a project and a group within it, for the whole UI.
 */
import { useQuery } from "@tanstack/react-query";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { Check, ChevronsUpDown, GitMerge, LayoutGrid } from "lucide-react";
import { useState } from "react";
import { api, type Flow, unwrap } from "@/api/client";
import { upstreamNames } from "@/components/flow-graph";
import { StateBar } from "@/components/state-bar";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { nestByProject } from "@/lib/groups";
import { useProject } from "@/lib/project";

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

/**
 * The scope picker at the top of the sidebar on the scoped pages: a button naming
 * the scope that opens a filterable list of every project and, under each, its
 * groups.
 */
export function ScopePicker() {
  const { project, group, pick: onPick, all, tree } = useScopeControl();
  const [open, setOpen] = useState(false);
  const pick = (p: string, g = "") => {
    onPick(p, g);
    setOpen(false);
  };
  const label = !project
    ? "All projects"
    : group
      ? `${project} › ${group === project ? "Ungrouped flows" : group}`
      : project;
  const count = !project
    ? all.length
    : (tree.find((p) => p.key === project)?.groups.find((g) => g.key === group)?.items.length ??
      (group === project ? tree.find((p) => p.key === project)?.rows.length : undefined) ??
      tree.find((p) => p.key === project)?.items.length ??
      0);
  return (
    <div className="flex flex-col gap-1.5" data-testid="scope-picker">
      <span className="px-2 text-xs font-medium text-muted-foreground">Project</span>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            aria-label={`Scope: ${label}`}
            className="flex h-9 w-full items-center gap-2 rounded-md border bg-card px-2.5 text-left text-sm hover:bg-accent"
            data-testid="scope-trigger"
          >
            <LayoutGrid className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="min-w-0 flex-1 truncate font-medium">{label}</span>
            <span className="text-xs text-muted-foreground">{count}</span>
            <ChevronsUpDown className="size-3.5 shrink-0 text-muted-foreground" />
          </button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-72 p-0">
          <Command>
            <CommandInput placeholder="Find a project or group" />
            <CommandList>
              <CommandEmpty>No project or group matches.</CommandEmpty>
              <CommandGroup>
                <CommandItem value="all projects" onSelect={() => pick("")} data-testid="scope-all">
                  <span className="flex-1">All projects</span>
                  {!project ? <Check className="size-3.5" /> : null}
                  <span className="text-xs text-muted-foreground">{all.length}</span>
                </CommandItem>
              </CommandGroup>
              <CommandGroup heading="Projects">
                {tree.flatMap((p) => {
                  // A project with only its own flows is one group; listing it again adds nothing.
                  const groups = [
                    ...(p.rows.length && p.groups.length
                      ? [{ key: p.key, label: "Ungrouped flows", items: p.rows }]
                      : []),
                    ...p.groups.map((g) => ({ key: g.key, label: g.key, items: g.items })),
                  ];
                  const projectActive = project === p.key && !group;
                  return [
                    <CommandItem
                      key={p.key}
                      value={`${p.key}`}
                      onSelect={() => pick(p.key)}
                      data-testid={`scope-project-${p.key}`}
                    >
                      <span className="min-w-0 flex-1 truncate font-medium">{p.key}</span>
                      {projectActive ? <Check className="size-3.5" /> : null}
                      <StateBar counts={lastStates(p.items)} className="w-7" height={4} />
                      <span className="min-w-3.5 text-right text-xs text-muted-foreground">
                        {p.items.length}
                      </span>
                    </CommandItem>,
                    ...groups.map((g) => (
                      <CommandItem
                        key={`${p.key}/${g.key}`}
                        value={`${p.key} ${g.label}`}
                        onSelect={() => pick(p.key, g.key)}
                        data-testid={`scope-group-${p.key}/${g.key}`}
                        className="pl-6"
                      >
                        <span className="min-w-0 flex-1 truncate">{g.label}</span>
                        {project === p.key && group === g.key ? <Check className="size-3.5" /> : null}
                        {g.items.some((f) => upstreamNames(f).length) ? (
                          <GitMerge className="size-3 text-muted-foreground" aria-label="has dependencies" />
                        ) : null}
                        <span className="min-w-3.5 text-right text-xs text-muted-foreground">
                          {g.items.length}
                        </span>
                      </CommandItem>
                    )),
                  ];
                })}
              </CommandGroup>
            </CommandList>
          </Command>
          <div className="border-t px-3 py-2 text-[11.5px] leading-4 text-muted-foreground">
            Groups come from <span className="font-mono">@flow(group=…)</span>.
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}
