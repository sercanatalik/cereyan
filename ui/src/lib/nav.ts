import {
  Activity,
  Bell,
  Boxes,
  GitBranch,
  LayoutDashboard,
  ListChecks,
  ListOrdered,
  type LucideIcon,
  Server,
  Settings,
  Variable,
} from "lucide-react";

export type NavItem = {
  to: string;
  label: string;
  icon: LucideIcon;
  /** Search parameters the entry opens with; Workers is a tab of the Queue page. */
  search?: Record<string, string>;
};

/** The sections, in the sidebar's groups and order. */
export const NAV_GROUPS: { label: string; items: NavItem[] }[] = [
  {
    label: "Operate",
    items: [
      { to: "/", label: "Dashboard", icon: LayoutDashboard },
      { to: "/runs", label: "Runs", icon: ListChecks },
      { to: "/queue", label: "Queue", icon: ListOrdered },
    ],
  },
  {
    label: "Build",
    items: [
      { to: "/flows", label: "Flows", icon: GitBranch },
      { to: "/variables", label: "Variables", icon: Variable },
      { to: "/rules", label: "Rules", icon: Bell },
    ],
  },
  {
    label: "Observe",
    items: [
      { to: "/events", label: "Events", icon: Activity },
      { to: "/artifacts", label: "Artifacts", icon: Boxes },
      { to: "/queue", label: "Workers", icon: Server, search: { tab: "workers" } },
    ],
  },
  { label: "System", items: [{ to: "/settings", label: "Settings", icon: Settings }] },
];

/** Every section, flat, in sidebar order. */
export const NAV: NavItem[] = NAV_GROUPS.flatMap((g) => g.items);

/** Whether `item` is the section the current location belongs to. */
export function isActive(item: NavItem, path: string, search: Record<string, unknown>): boolean {
  const here = item.to === "/" ? path === "/" : path === item.to || path.startsWith(`${item.to}/`);
  if (!here) return false;
  if (item.to !== "/queue") return true;
  // Queue and Workers share a route; the tab tells them apart.
  return (search.tab === "workers") === (item.search?.tab === "workers");
}
