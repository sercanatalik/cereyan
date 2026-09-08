import { useQuery } from "@tanstack/react-query";
import { Link, useRouterState } from "@tanstack/react-router";
import { ChevronDown, ChevronRight, Moon, Search, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { api, unwrap } from "@/api/client";
import { CommandPalette } from "@/components/command-palette";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Kbd } from "@/components/ui/kbd";
import { useLiveUpdates } from "@/lib/live";
import { NAV } from "@/lib/nav";
import { useProject } from "@/lib/project";
import { cn } from "@/lib/utils";

export function useTheme() {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
    try {
      localStorage.setItem("cereyan-theme", dark ? "dark" : "light");
    } catch {}
  }, [dark]);
  return { dark, toggle: () => setDark((d) => !d) };
}

/** The brand mark: an ink tile with a current running through it. */
export function Mark({ className }: { className?: string }) {
  return (
    <svg width="18" height="18" viewBox="0 0 18 18" className={cn("shrink-0", className)} aria-hidden="true">
      <rect width="18" height="18" rx="5" className="fill-primary" />
      <path
        d="M4 11.5 7.5 5l1.5 6.5L12.5 5l1.5 6.5"
        className="stroke-primary-foreground"
        strokeWidth="1.6"
        fill="none"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function LiveIndicator() {
  const { status } = useLiveUpdates();
  const color =
    status === "live" ? "bg-emerald-500" : status === "reconnecting" ? "bg-red-500" : "bg-amber-400";
  return (
    <span
      className="flex h-7 items-center gap-1.5 px-2 text-xs text-muted-foreground"
      data-testid="live-indicator"
      data-status={status}
    >
      <span
        className={cn(
          "h-[7px] w-[7px] rounded-full",
          color,
          status === "live" && "shadow-[0_0_0_3px_rgba(16,185,129,0.18)]",
          status !== "live" && "animate-pulse",
        )}
      />
      {status}
    </span>
  );
}

function ProjectSwitcher() {
  const { scope, setProject } = useProject();
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const projects = Array.from(new Set((flows.data ?? []).map((f) => f.project))).sort();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="outline"
          size="sm"
          className="h-[30px] gap-2 font-medium"
          data-testid="project-switcher"
        >
          <span className="font-normal text-muted-foreground">Project</span>
          {scope || "All"}
          <ChevronDown className="size-3.5 text-muted-foreground" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-44">
        <DropdownMenuRadioGroup value={scope} onValueChange={setProject}>
          <DropdownMenuRadioItem value="">All projects</DropdownMenuRadioItem>
          {projects.map((p) => (
            <DropdownMenuRadioItem key={p} value={p}>
              {p}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

export function Shell({ children }: { children: React.ReactNode }) {
  const { dark, toggle } = useTheme();
  const path = useRouterState({ select: (s) => s.location.pathname });
  const [paletteOpen, setPaletteOpen] = useState(false);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setPaletteOpen((o) => !o);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return (
    <div className="flex h-full min-w-[960px] flex-col">
      <header className="flex h-[52px] shrink-0 items-center gap-7 border-b bg-card px-6">
        <Link to="/" className="flex items-center gap-2 text-sm font-semibold tracking-tight">
          <Mark /> cereyan
        </Link>
        <nav className="flex h-full items-stretch gap-0.5" aria-label="Sections">
          {NAV.map((item) => {
            const active = item.to === "/" ? path === "/" : path.startsWith(item.to);
            return (
              <Link
                key={item.to}
                to={item.to}
                aria-current={active ? "page" : undefined}
                className={cn(
                  "-mb-px flex items-center border-b-2 border-transparent px-2.5 text-sm font-medium text-muted-foreground hover:text-foreground",
                  active && "border-foreground text-foreground",
                )}
              >
                {item.label}
              </Link>
            );
          })}
        </nav>
        <div className="ml-auto flex items-center gap-2">
          <ProjectSwitcher />
          <Button
            variant="outline"
            size="sm"
            className="h-[30px] w-60 justify-start gap-2 font-normal text-muted-foreground"
            onClick={() => setPaletteOpen(true)}
            aria-label="Search"
          >
            <Search className="size-3.5" />
            <span className="flex-1 text-left">Search runs, flows, artifacts</span>
            <Kbd>{isMac ? "⌘K" : "Ctrl K"}</Kbd>
          </Button>
          <LiveIndicator />
          <Button variant="ghost" size="icon-sm" aria-label="Toggle theme" onClick={toggle}>
            {dark ? <Sun className="size-4" /> : <Moon className="size-4" />}
          </Button>
        </div>
      </header>
      <main className="flex min-w-0 flex-1 flex-col overflow-auto">{children}</main>
      <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
    </div>
  );
}

/** The small "Runs › name" line at the top of a detail page. */
export function Crumb({ items }: { items: { label: string; to?: string }[] }) {
  return (
    <nav className="flex items-center gap-1.5 text-xs text-muted-foreground" aria-label="Breadcrumb">
      {items.map((item, i) => (
        <span key={`${item.to ?? ""}#${item.label}`} className="flex items-center gap-1.5">
          {i > 0 ? <ChevronRight className="size-3" /> : null}
          {item.to ? (
            <Link to={item.to} className="hover:text-foreground">
              {item.label}
            </Link>
          ) : (
            <span className="text-foreground">{item.label}</span>
          )}
        </span>
      ))}
    </nav>
  );
}

/**
 * A page body. List pages pass `title`; detail pages pass `crumbs` and draw
 * their own header. `wide` drops the 1280 px column for full-width layouts.
 */
export function Page({
  crumbs,
  title,
  subtitle,
  actions,
  wide,
  className,
  children,
}: {
  crumbs?: { label: string; to?: string }[];
  title?: React.ReactNode;
  subtitle?: React.ReactNode;
  actions?: React.ReactNode;
  wide?: boolean;
  className?: string;
  children: React.ReactNode;
}) {
  const showCrumb = crumbs && crumbs.length > 1;
  return (
    <div
      className={cn(
        "flex min-h-full w-full flex-col gap-5",
        wide ? "p-0" : "mx-auto max-w-[1280px] px-8 pt-6 pb-10",
        className,
      )}
    >
      {showCrumb ? (
        <div className="flex items-center justify-between gap-4">
          <Crumb items={crumbs} />
          {!title && actions ? <div className="flex items-center gap-2">{actions}</div> : null}
        </div>
      ) : null}
      {title || (actions && !showCrumb) ? (
        <div className="flex items-end justify-between gap-4">
          <div className="flex flex-col gap-0.5">
            {title ? <h1 className="text-xl font-semibold tracking-tight">{title}</h1> : null}
            {subtitle ? <div className="text-sm text-muted-foreground">{subtitle}</div> : null}
          </div>
          {actions ? <div className="flex items-center gap-2">{actions}</div> : null}
        </div>
      ) : null}
      {children}
    </div>
  );
}
