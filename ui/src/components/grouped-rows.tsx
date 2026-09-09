/**
 * Collapsible groups inside one table. The header row is a column-aligned
 * rollup of the rows beneath it, so collapsing reads as zooming out: nothing
 * re-flows, and the header stays informative while expanded.
 */
import { ChevronRight, TriangleAlert } from "lucide-react";
import { type ReactNode, useId, useRef, useState } from "react";
import type { StateType } from "@/api/client";
import { DOT_COLORS } from "@/components/ported/state-badge";
import { StateBar, type StateCounts } from "@/components/state-bar";
import { Td, Tr } from "@/components/ui/table";
import type { Group } from "@/lib/groups";
import { cn } from "@/lib/utils";

/** A group holding more rows than this starts collapsed. */
export const COLLAPSE_OVER = 5;

/**
 * Open state per group, by the precedence: an explicit toggle, then an active
 * search, then being the only group, then the row count.
 *
 * The default is fixed the first time a group is seen and never recomputed, so
 * a live update can neither collapse a group being read nor open one that was
 * closed. Toggles live for the mount only; nothing is persisted.
 */
export function useGroupOpen<T>(groups: Group<T>[], searchActive = false) {
  const [toggled, setToggled] = useState<Record<string, boolean>>({});
  const defaults = useRef<Record<string, boolean>>({});
  for (const g of groups) {
    if (!(g.key in defaults.current)) {
      defaults.current[g.key] = groups.length === 1 || g.items.length <= COLLAPSE_OVER;
    }
  }
  return {
    isOpen: (key: string) => toggled[key] ?? (searchActive ? true : (defaults.current[key] ?? true)),
    toggle: (key: string, open: boolean) => setToggled((t) => ({ ...t, [key]: open })),
  };
}

/** "12", or "3 of 12" while a filter or search hides part of the group. */
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
 * The group's states as a bar with counts, and — as a separate channel — the
 * flows the running server no longer has registered. A group whose flows all
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

/** The union of the group's tags, the first few and a count of the rest. */
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
 * One group: a header row that toggles, and the group's rows in a `<tbody>` of
 * their own so the header can point at them with `aria-controls`.
 *
 * Radix `Collapsible` is not used: it renders a wrapper element, and sibling
 * `<tr>`s cannot be wrapped in a `<div>`. The cost is no height animation.
 */
export function GroupSection<T>({
  group,
  open,
  onOpenChange,
  identity,
  leading,
  rollup,
  children,
}: {
  group: Group<T>;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Under the group name: the projects it spans, or where it came from. */
  identity?: ReactNode;
  /** Rollup cells that sit before the name column, when a table has any. */
  leading?: ReactNode;
  /** The rollup cells after the name, column-aligned with the rows below. */
  rollup: ReactNode;
  children: ReactNode;
}) {
  const bodyId = `${useId()}-rows`;
  const toggle = () => onOpenChange(!open);
  return (
    <>
      <tbody>
        <Tr
          className="cursor-pointer border-b bg-muted/60 hover:bg-muted"
          onClick={toggle}
          data-testid={`group-${group.key}`}
          data-open={open}
        >
          {leading}
          <Td className="h-[38px] font-medium">
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
                <span className="font-semibold">{group.key}</span>
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
