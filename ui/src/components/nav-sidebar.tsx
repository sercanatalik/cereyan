import { useQuery } from "@tanstack/react-query";
import { Link, useRouterState } from "@tanstack/react-router";
import { api, unwrap } from "@/api/client";
import { ScopePicker } from "@/components/scope-sidebar";
import { isActive, NAV_GROUPS, type NavItem } from "@/lib/nav";
import { useProject } from "@/lib/project";
import { cn } from "@/lib/utils";

/** The queue depth on the Queue entry, hidden while nothing waits. */
function QueueBadge() {
  const server = useQuery({
    queryKey: ["server"],
    queryFn: async () => unwrap(await api.GET("/api/server")),
    refetchInterval: 5000,
  });
  const queued = server.data?.queued ?? 0;
  if (queued <= 0) return null;
  return (
    <span
      className="rounded-full bg-foreground px-1.5 font-mono text-[11px] leading-[18px] text-background"
      title={`${queued} in line`}
      data-testid="queue-badge"
    >
      {queued}
    </span>
  );
}

/** How many flows the scope holds, beside the Flows entry. */
function FlowCount() {
  const { scope, group } = useProject();
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  if (!flows.data) return null;
  const n = flows.data.filter(
    (f) => (!scope || f.project === scope) && (!group || (f.group ?? f.project) === group),
  ).length;
  return <span className="font-mono text-xs text-muted-foreground">{n}</span>;
}

function Entry({ item, active }: { item: NavItem; active: boolean }) {
  return (
    <Link
      to={item.to}
      search={(item.search ?? {}) as any}
      aria-current={active ? "page" : undefined}
      // The router marks a link to the current path itself; matching exactly, search included,
      // keeps it from marking Queue on the Workers tab, which shares its path.
      activeOptions={{ exact: true, includeSearch: true }}
      className={cn(
        "flex h-8 items-center gap-2.5 rounded-md px-2.5 text-sm text-foreground/80 hover:bg-accent hover:text-foreground",
        active && "bg-card font-semibold text-foreground shadow-[0_0_0_1px_var(--border)] hover:bg-card",
      )}
    >
      <item.icon className={cn("size-4 shrink-0 text-muted-foreground", active && "text-foreground")} />
      <span className="flex-1">{item.label}</span>
      {item.label === "Queue" ? <QueueBadge /> : null}
      {item.label === "Flows" ? <FlowCount /> : null}
    </Link>
  );
}

/**
 * The sidebar on every page: the scope picker on the scoped pages, then the
 * sections in their groups.
 */
export function NavSidebar({ scoped }: { scoped: boolean }) {
  const location = useRouterState({ select: (s) => s.location });
  const search = (location.search ?? {}) as Record<string, unknown>;
  return (
    <aside
      aria-label="Sidebar"
      className="flex min-h-0 flex-col gap-5 overflow-auto border-r bg-muted/50 px-3 py-4"
      data-testid="nav-sidebar"
    >
      {scoped ? <ScopePicker /> : null}
      <nav aria-label="Sections" className="flex flex-col gap-5">
        {NAV_GROUPS.map((group) => (
          <div key={group.label} className="flex flex-col gap-0.5">
            <div className="px-2.5 pb-1 text-xs font-medium text-muted-foreground">{group.label}</div>
            {group.items.map((item) => (
              <Entry key={item.label} item={item} active={isActive(item, location.pathname, search)} />
            ))}
          </div>
        ))}
      </nav>
    </aside>
  );
}
