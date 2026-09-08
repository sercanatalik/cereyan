import {
  Activity,
  Bell,
  Boxes,
  GitBranch,
  LayoutDashboard,
  ListChecks,
  Settings,
  Variable,
} from "lucide-react";

/** The eight sections, in top-bar order. */
export const NAV = [
  { to: "/", label: "Dashboard", icon: LayoutDashboard },
  { to: "/runs", label: "Runs", icon: ListChecks },
  { to: "/flows", label: "Flows", icon: GitBranch },
  { to: "/events", label: "Events", icon: Activity },
  { to: "/artifacts", label: "Artifacts", icon: Boxes },
  { to: "/rules", label: "Rules", icon: Bell },
  { to: "/variables", label: "Variables", icon: Variable },
  { to: "/settings", label: "Settings", icon: Settings },
] as const;
