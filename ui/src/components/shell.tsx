import { useQuery } from "@tanstack/react-query";
import { Link, useRouterState } from "@tanstack/react-router";
import { ChevronRight, Moon, Search, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { api, unwrap } from "@/api/client";
import { CommandPalette } from "@/components/command-palette";
import { PauseBanner } from "@/components/pause-banner";
import { ScopeSidebar } from "@/components/scope-sidebar";
import { Button } from "@/components/ui/button";
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

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

// The server writes the UI title into index.html's <title>, so the first paint needs no request.
const INITIAL_TITLE = (typeof document !== "undefined" && document.title) || "cereyan";

/** The UI title (`[ui] title` in cereyan.toml), kept in step with the browser tab. */
export function useUiTitle(): string {
  const server = useQuery({
    queryKey: ["server"],
    queryFn: async () => unwrap(await api.GET("/api/server")),
  });
  const title = server.data?.title ?? INITIAL_TITLE;
  useEffect(() => {
    document.title = title;
  }, [title]);
  return title;
}

const SECURE_GUIDE = "https://sercanatalik.github.io/cereyan/guides/secure-the-server/";

/**
 * Shown while the server is bound beyond loopback with no token required, which
 * only happens when the operator started it with `allow_unauthenticated`. It is
 * not dismissible: the state it names is the reason it is there.
 */
export function ExposedBanner() {
  const server = useQuery({
    queryKey: ["server"],
    queryFn: async () => unwrap(await api.GET("/api/server")),
  });
  if (!server.data?.exposed) return null;
  return (
    <div
      role="alert"
      className="shrink-0 border-b border-amber-400/60 bg-amber-50 px-6 py-1.5 text-sm dark:bg-amber-950/30"
      data-testid="exposed-banner"
    >
      <span className="font-medium">This server is reachable from the network without a token. </span>
      Anyone who can reach its port can start runs and change settings.{" "}
      <a href={SECURE_GUIDE} className="underline underline-offset-2" target="_blank" rel="noreferrer">
        Secure the server
      </a>
    </div>
  );
}

/** The list pages the project scope narrows, which carry the scope sidebar. */
const SCOPED = ["/", "/runs", "/flows", "/events", "/artifacts"];

export function Shell({ children }: { children: React.ReactNode }) {
  const { dark, toggle } = useTheme();
  const path = useRouterState({ select: (s) => s.location.pathname });
  const [paletteOpen, setPaletteOpen] = useState(false);
  const title = useUiTitle();
  const { scope, group } = useProject();
  const scoped = SCOPED.includes(path.replace(/(.)\/$/, "$1"));
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
    <div
      className="flex h-full min-w-[960px] flex-col"
      data-testid="shell"
      data-scope-project={scope}
      data-scope-group={group}
    >
      <header className="shrink-0 border-b bg-card">
        <div className="frame flex h-[52px] items-center gap-7" data-testid="top-bar-row">
          <Link to="/" className="flex min-w-0 items-center gap-2 text-sm font-semibold tracking-tight">
            <Mark />
            <span className="max-w-60 truncate" title={title} data-testid="ui-title">
              {title}
            </span>
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
        </div>
      </header>
      <ExposedBanner />
      <PauseBanner />
      {scoped ? (
        <div className="grid min-h-0 flex-1 grid-cols-[248px_minmax(0,1fr)]">
          <ScopeSidebar />
          <main className="flex min-w-0 flex-col overflow-auto">{children}</main>
        </div>
      ) : (
        <main className="flex min-w-0 flex-1 flex-col overflow-auto">{children}</main>
      )}
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
 * How much of the frame a page uses: `fluid` fills it, `narrow` caps the content
 * at 1080 px on the frame's left edge, and `bleed` runs edge to edge, leaving the
 * page to put its own rows on the frame.
 */
export type PageWidth = "fluid" | "narrow" | "bleed";

/**
 * A page body. List pages pass `title`; detail pages pass `crumbs` and draw
 * their own header.
 */
export function Page({
  crumbs,
  title,
  subtitle,
  actions,
  width = "fluid",
  className,
  children,
}: {
  crumbs?: { label: string; to?: string }[];
  title?: React.ReactNode;
  subtitle?: React.ReactNode;
  actions?: React.ReactNode;
  width?: PageWidth;
  className?: string;
  children: React.ReactNode;
}) {
  const showCrumb = crumbs && crumbs.length > 1;
  const body = (
    <>
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
    </>
  );
  if (width === "narrow") {
    // Capped but not centred inside the frame, so the title keeps the left edge.
    return (
      <div className={cn("frame flex min-h-full flex-col pt-6 pb-10", className)} data-width="narrow">
        <div className="flex w-full max-w-[1080px] flex-col gap-5">{body}</div>
      </div>
    );
  }
  return (
    <div
      className={cn(
        "flex min-h-full w-full flex-col gap-5",
        width === "bleed" ? "p-0" : "frame pt-6 pb-10",
        className,
      )}
      data-width={width}
    >
      {body}
    </div>
  );
}
