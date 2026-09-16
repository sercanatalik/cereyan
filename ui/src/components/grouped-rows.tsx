/**
 * Collapsible sections inside one table, nested project then group. A header
 * row is a column-aligned rollup of the rows beneath it, so collapsing reads as
 * zooming out: nothing re-flows, and the header stays informative while
 * expanded.
 */
import { ChevronRight, TriangleAlert } from "lucide-react";
import { type ReactNode, useId, useRef, useState } from "react";
import type { StateType } from "@/api/client";
import { DOT_COLORS } from "@/components/ported/state-badge";
import { StateBar, type StateCounts } from "@/components/state-bar";
import { Td, Tr } from "@/components/ui/table";
import type { ProjectGroup } from "@/lib/groups";
import { cn } from "@/lib/utils";

/** A section holding more rows than this starts collapsed. */
export const COLLAPSE_OVER = 5;

/** One section's input to the open-state ladder. */
export interface OpenSection {
  openKey: string;
  /** Rows beneath this section, for the collapse threshold. */
  count: number;
  /** The only section at its level, which expands it however large. */
  lone: boolean;
}

/**
 * Every section of a nested list, both levels. A project is lone when it is the
 * only project; a group is lone when it is its project's only group and the
 * project keeps no rows of its own, so the pair reads as one section.
 */
export function openSections<T>(projects: ProjectGroup<T>[]): OpenSection[] {
  const out: OpenSection[] = [];
  for (const p of projects) {
    out.push({ openKey: p.openKey, count: p.items.length, lone: projects.length === 1 });
    for (const g of p.groups) {
      out.push({
        openKey: g.openKey,
        count: g.items.length,
        lone: p.groups.length === 1 && p.rows.length === 0,
      });
    }
  }
  return out;
}

/**
 * Open state per section, by the precedence: an explicit toggle, then an active
 * search, then being the only section at its level, then the row count.
 *
 * The default is fixed the first time a section is seen and never recomputed,
 * so a live update can neither collapse a section being read nor open one that
 * was closed. Toggles live for the mount only; nothing is persisted. Keys carry
 * their level and project, so one group name in two projects toggles apart.
 */
export function useGroupOpen(sections: OpenSection[], searchActive = false) {
  const [toggled, setToggled] = useState<Record<string, boolean>>({});
  const defaults = useRef<Record<string, boolean>>({});
  for (const s of sections) {
    if (!(s.openKey in defaults.current)) {
      defaults.current[s.openKey] = s.lone || s.count <= COLLAPSE_OVER;
    }
  }
  return {
    isOpen: (key: string) => toggled[key] ?? (searchActive ? true : (defaults.current[key] ?? true)),
    toggle: (key: string, open: boolean) => setToggled((t) => ({ ...t, [key]: open })),
  };
}

/** "12", or "3 of 12" while a filter or search hides part of the section. */
export function GroupCount({ shown, total }: { shown: number; total?: number }) {
  const hidden = total !== undefined && total > shown;
  return (
    <span className="tabular-nums text-muted-foreground" data-testid="group-count">
      {hidden ? `${shown} of ${total}` : shown}
    </span>
  );
}

const COUNT_ORDER: StateType[] = ["Completed", "Failed", "Crashed", "Running", "Paused"];

/**
 * The section's states as a bar with counts, and — as a separate channel — the
 * flows the running server no longer has registered. A section whose flows all
 * last succeeded but have since deregistered must not read as healthy.
 */
export function GroupStateRollup({
  counts,
  stale,
  className,
}: {
  counts: StateCounts;
  stale?: number;
  className?: string;
}) {
  const total = Object.values(counts).reduce<number>((a, n) => a + (n ?? 0), 0);
  const named = [...COUNT_ORDER, ...Object.keys(counts).filter((k) => !COUNT_ORDER.includes(k as StateType))]
    .map((k) => [k, counts[k] ?? 0] as const)
    .filter(([, n]) => n > 0);
  const label = named.map(([k, n]) => `${n} ${k}`).join(", ");
  return (
    <span className={cn("flex items-center gap-2.5", className)} data-testid="group-rollup">
      {total > 0 ? <StateBar counts={counts} className="w-24" height={6} title={label} /> : null}
      <span className="flex items-center gap-2 text-xs text-muted-foreground">
        {named.map(([k, n]) => (
          <span key={k} className="inline-flex items-center gap-1 tabular-nums" data-state={k}>
            <span
              className={cn("size-1.5 rounded-full", DOT_COLORS[k as StateType] ?? "bg-muted-foreground")}
            />
            {n}
          </span>
        ))}
      </span>
      {stale ? (
        <span
          className="inline-flex items-center gap-1 text-xs text-amber-700 dark:text-amber-400"
          data-testid="group-stale"
          title={`${stale} not registered by this server`}
        >
          <TriangleAlert className="size-3.5" />
          {stale} stale
        </span>
      ) : null}
    </span>
  );
}

/** The union of the section's tags, the first few and a count of the rest. */
export function GroupTags({ tags, limit = 3 }: { tags: string[]; limit?: number }) {
  if (tags.length === 0) return null;
  const shown = tags.slice(0, limit);
  return (
    <span className="flex items-center gap-1" data-testid="group-tags">
      {shown.map((t) => (
        <span
          key={t}
          className="inline-flex h-5 items-center rounded-[5px] border bg-background px-1.5 text-[11.5px] font-medium"
        >
          {t}
        </span>
      ))}
      {tags.length > shown.length ? (
        <span className="text-[11.5px] text-muted-foreground">+{tags.length - shown.length}</span>
      ) : null}
    </span>
  );
}

/**
 * One section: a header row that toggles, and its rows in a `<tbody>` of their
 * own so the header can point at them with `aria-controls`. A project's groups
 * render as sibling sections after it rather than inside its `<tbody>`, because
 * a `<tbody>` cannot nest; a collapsed project omits them, its header having
 * already rolled up everything beneath it.
 *
 * Radix `Collapsible` is not used: it renders a wrapper element, and sibling
 * `<tr>`s cannot be wrapped in a `<div>`. The cost is no height animation.
 */
export function GroupSection({
  name,
  testId,
  open,
  onOpenChange,
  identity,
  leading,
  rollup,
  indent = false,
  children,
}: {
  /** The name shown in the header. */
  name: string;
  testId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Under the name: where the rows came from, when it says something the name does not. */
  identity?: ReactNode;
  /** Rollup cells that sit before the name column, when a table has any. */
  leading?: ReactNode;
  /** The rollup cells after the name, column-aligned with the rows below. */
  rollup: ReactNode;
  /** A group sits one level inside its project. */
  indent?: boolean;
  children: ReactNode;
}) {
  const bodyId = `${useId()}-rows`;
  const toggle = () => onOpenChange(!open);
  return (
    <>
      <tbody>
        <Tr
          className={cn("cursor-pointer border-b hover:bg-muted", indent ? "bg-muted/30" : "bg-muted/60")}
          onClick={toggle}
          data-testid={testId}
          data-open={open}
        >
          {leading}
          <Td className={cn("h-[38px] font-medium", indent && "pl-7")}>
            <button
              type="button"
              aria-expanded={open}
              aria-controls={bodyId}
              onClick={(e) => {
                e.stopPropagation();
                toggle();
              }}
              className="-my-1 flex items-center gap-1.5 rounded-sm py-1 text-left focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
            >
              <ChevronRight
                className={cn("size-3.5 text-muted-foreground transition-transform", open && "rotate-90")}
              />
              <span className="flex flex-col gap-px">
                <span className={cn(indent ? "font-medium" : "font-semibold")}>{name}</span>
                {identity ? (
                  <span className="text-[11.5px] font-normal text-muted-foreground">{identity}</span>
                ) : null}
              </span>
            </button>
          </Td>
          {rollup}
        </Tr>
      </tbody>
      <tbody id={bodyId} hidden={!open}>
        {children}
      </tbody>
    </>
  );
}
