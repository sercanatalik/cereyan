import { useQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { DateRangeSelect, type RangePreset, rangeStart } from "@/components/ported/date-range";
import { JsonView } from "@/components/ported/json-view";
import { DOT_COLORS } from "@/components/ported/state-badge";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/input";
import { useLiveEvent } from "@/lib/live";
import { useElementSize, useMinWidth } from "@/lib/media";
import { useProject } from "@/lib/project";
import { cn, formatTime } from "@/lib/utils";

type Event = components["schemas"]["Event"];
type Search = { name?: string; kind?: string; flow?: string };

export const Route = createFileRoute("/events")({
  validateSearch: (s: Record<string, unknown>): Search => ({
    name: typeof s.name === "string" ? s.name : undefined,
    kind: typeof s.kind === "string" ? s.kind : undefined,
    flow: typeof s.flow === "string" ? s.flow : undefined,
  }),
  component: EventsPage,
});

const ROW = 36;
const DAY_ROW = 28;
/** The virtual window's height until the feed is measured (jsdom never measures it). */
const FALLBACK_HEIGHT = 560;
const OVERSCAN = 5 * ROW;
/** From this viewport width the detail panel stays open beside the feed. */
const PANEL_FROM = 1440;

// The dot of an event whose name ends in a state takes that state's colour.
const SUFFIX_DOTS: Record<string, string> = {
  scheduled: DOT_COLORS.Scheduled,
  pending: DOT_COLORS.Pending,
  running: DOT_COLORS.Running,
  completed: DOT_COLORS.Completed,
  failed: DOT_COLORS.Failed,
  crashed: DOT_COLORS.Crashed,
  cancelled: DOT_COLORS.Cancelled,
  paused: DOT_COLORS.Paused,
  late: "bg-orange-500",
  retrying: "bg-amber-400",
  skipped: "bg-teal-500",
  cached: "bg-cyan-500",
};

/** The dot class for an event name: its state's colour, or neutral when it names none. */
export function eventDotClass(name: string): string {
  return SUFFIX_DOTS[name.slice(name.lastIndexOf(".") + 1)] ?? "bg-muted-foreground/40";
}

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** "Today", "Yesterday", or the weekday, and the date, for the day an event happened. */
export function dayLabel(micros: number, now = Date.now()): { label: string; date: string } {
  const d = new Date(micros / 1000);
  const day = startOfDay(d.getTime());
  const today = startOfDay(now);
  const label =
    day === today
      ? "Today"
      : day === startOfDay(today - 1)
        ? "Yesterday"
        : d.toLocaleDateString(undefined, { weekday: "long" });
  return { label, date: d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" }) };
}

type Line = { key: string; day: { label: string; date: string } } | { key: string; event: Event };

/** The feed as rows: a day row before each day's events, newest first. */
function toLines(events: Event[], now: number): Line[] {
  const lines: Line[] = [];
  let current = Number.NaN;
  for (const event of events) {
    const day = startOfDay(event.occurred / 1000);
    if (day !== current) {
      current = day;
      lines.push({ key: `day:${day}`, day: dayLabel(event.occurred, now) });
    }
    lines.push({ key: `event:${event.id}`, event });
  }
  return lines;
}

/** The index of the last line starting at or above `y`. */
function lineAt(offsets: number[], y: number): number {
  let lo = 0;
  let hi = offsets.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (offsets[mid] <= y) lo = mid;
    else hi = mid - 1;
  }
  return Math.max(0, lo);
}

const timeOfDay = (micros: number) =>
  new Date(micros / 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });

function preview(payload: unknown): string {
  const text = JSON.stringify(payload ?? {});
  return text.length > 300 ? `${text.slice(0, 300)}…` : text;
}

function EventsPage() {
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const set = (patch: Partial<Search>) => navigate({ search: (old) => ({ ...old, ...patch }) });
  const [range, setRange] = useState<RangePreset>("24h");
  const [selected, setSelected] = useState<Event | null>(null);
  const [live, setLive] = useState<Event[]>([]);
  const [scrollTop, setScrollTop] = useState(0);
  const [listRef, listSize] = useElementSize<HTMLDivElement>();
  const anchor = useRef<{ key: string; delta: number } | null>(null);
  const persistent = useMinWidth(PANEL_FROM);
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const { project } = useProject();
  const scopedFlows = (flows.data ?? []).filter((f) => !project || f.project === project);
  const flowId = search.flow ? flows.data?.find((f) => f.name === search.flow)?.id : undefined;
  const query = useQuery({
    queryKey: ["events", search.name, search.kind, flowId, range],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/events", {
          params: {
            query: {
              name: search.name,
              resource_kind: search.kind,
              flow_id: flowId,
              after: rangeStart(range),
              limit: 500,
            },
          },
        }),
      ),
  });
  const fetched = query.dataUpdatedAt;
  useEffect(() => {
    if (fetched) setLive([]);
  }, [fetched]);
  useLiveEvent("event.created", (data: Event) => {
    if (
      search.name &&
      !(search.name.endsWith("*")
        ? data.name.startsWith(search.name.slice(0, -1))
        : data.name === search.name)
    )
      return;
    if (search.kind && data.resource.kind !== search.kind) return;
    setLive((old) => [data, ...old].slice(0, 200));
  });
  // The events route has no project filter; runs and flows of other projects drop out here.
  const events = useMemo(() => {
    const scoped = project
      ? new Set((flows.data ?? []).filter((f) => f.project === project).map((f) => f.id))
      : null;
    return [...live, ...(query.data?.items ?? [])].filter(
      (e) => !scoped || e.flow_id == null || scoped.has(e.flow_id),
    );
  }, [live, query.data, project, flows.data]);
  const lines = useMemo(() => toLines(events, Date.now()), [events]);
  const offsets = useMemo(() => {
    const out: number[] = [];
    let y = 0;
    for (const line of lines) {
      out.push(y);
      y += "day" in line ? DAY_ROW : ROW;
    }
    out.push(y);
    return out;
  }, [lines]);
  const total = offsets[offsets.length - 1] ?? 0;
  // Live events prepend; keep the row the user was reading where it was.
  useLayoutEffect(() => {
    const el = listRef.current;
    const a = anchor.current;
    if (!el || !a || el.scrollTop <= 0) return;
    const i = lines.findIndex((l) => l.key === a.key);
    if (i < 0) return;
    const want = offsets[i] + a.delta;
    if (Math.abs(want - el.scrollTop) >= 1) {
      el.scrollTop = want;
      setScrollTop(want);
    }
  }, [lines, offsets, listRef]);
  const height = listSize.height || FALLBACK_HEIGHT;
  const first = lines.length ? lineAt(offsets, scrollTop - OVERSCAN) : 0;
  const last = lines.length ? Math.min(lines.length, lineAt(offsets, scrollTop + height + OVERSCAN) + 1) : 0;
  const onScroll = (el: HTMLDivElement) => {
    const top = el.scrollTop;
    setScrollTop(top);
    const i = lineAt(offsets, top);
    anchor.current = lines[i] ? { key: lines[i].key, delta: top - offsets[i] } : null;
  };
  return (
    <Page crumbs={[{ label: "Events" }]} title="Events" className="h-full min-h-0 pb-6">
      <div className="flex flex-wrap items-center gap-2">
        <Input
          placeholder="Name or prefix, e.g. run.*"
          value={search.name ?? ""}
          onChange={(e) => set({ name: e.target.value || undefined })}
          className="max-w-xs"
        />
        <Select
          value={search.kind ?? ""}
          onChange={(e) => set({ kind: e.target.value || undefined })}
          aria-label="Resource kind"
        >
          <option value="">Any resource</option>
          {["run", "task_run", "flow", "schedule", "rule", "custom"].map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </Select>
        <Select
          value={search.flow ?? ""}
          onChange={(e) => set({ flow: e.target.value || undefined })}
          aria-label="Flow"
        >
          <option value="">Any flow</option>
          {scopedFlows.map((f) => (
            <option key={f.id} value={f.name}>
              {f.project}/{f.name}
            </option>
          ))}
        </Select>
        <DateRangeSelect value={range} onChange={setRange} />
        <span className="ml-auto text-xs text-muted-foreground">{events.length} events</span>
      </div>
      <div className="flex min-h-0 flex-1 gap-3">
        <div
          ref={listRef}
          className="min-h-0 min-w-0 flex-1 overflow-auto rounded-md border bg-card"
          onScroll={(e) => onScroll(e.target as HTMLDivElement)}
          data-testid="event-feed"
        >
          <div style={{ height: total, position: "relative" }}>
            {lines.slice(first, last).map((line, i) => {
              const top = offsets[first + i];
              if ("day" in line)
                return (
                  <div
                    key={line.key}
                    className="absolute left-0 flex w-full items-center gap-2 border-b bg-muted/50 px-3 text-xs"
                    style={{ top, height: DAY_ROW }}
                    data-testid="day-row"
                  >
                    <span className="font-medium">{line.day.label}</span>
                    <span className="text-muted-foreground">{line.day.date}</span>
                  </div>
                );
              const e = line.event;
              return (
                <button
                  type="button"
                  key={line.key}
                  onClick={() => setSelected(e)}
                  className={cn(
                    "absolute left-0 flex w-full items-center gap-3 border-b px-3 text-left text-sm hover:bg-accent/50",
                    selected?.id === e.id && "bg-accent",
                  )}
                  style={{ top, height: ROW }}
                  data-event-name={e.name}
                >
                  <span className="w-16 shrink-0 text-xs tabular-nums text-muted-foreground">
                    {timeOfDay(e.occurred)}
                  </span>
                  <span className="flex w-52 shrink-0 items-center gap-2">
                    <span
                      className={cn("size-[7px] shrink-0 rounded-full", eventDotClass(e.name))}
                      data-testid="event-dot"
                    />
                    <span className="truncate font-mono text-xs">{e.name}</span>
                  </span>
                  <span className="w-16 shrink-0 text-xs text-muted-foreground">{e.resource.kind}</span>
                  <span className="w-56 shrink-0 truncate min-[1920px]:w-72">{e.resource.name}</span>
                  <span className="w-36 shrink-0 truncate text-xs text-muted-foreground min-[1920px]:w-44">
                    {e.related?.find((r) => r.kind === "flow")?.id ?? ""}
                  </span>
                  <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">
                    {preview(e.payload)}
                  </span>
                </button>
              );
            })}
            {events.length === 0 ? <div className="p-6 text-muted-foreground">No events</div> : null}
          </div>
        </div>
        {persistent || selected ? (
          <aside
            className="w-96 shrink-0 space-y-3 overflow-auto rounded-md border bg-card p-3 min-[1920px]:w-[480px]"
            data-testid="event-drawer"
          >
            {selected ? (
              <EventDetail event={selected} />
            ) : (
              <div className="text-sm text-muted-foreground">Select an event</div>
            )}
          </aside>
        ) : null}
      </div>
    </Page>
  );
}

function EventDetail({ event }: { event: Event }) {
  const nav = useNavigate();
  return (
    <>
      <div className="font-mono text-sm">{event.name}</div>
      <div className="text-xs text-muted-foreground">
        {formatTime(event.occurred)} · seq {event.seq} · {event.resource.kind} {event.resource.name}
      </div>
      <JsonView value={event.payload} wrap />
      <Button
        size="sm"
        onClick={() =>
          nav({
            to: "/rules",
            search: {
              new: "1",
              events: event.name,
              flow: event.related?.find((r) => r.kind === "flow")?.name ?? "",
            } as any,
          })
        }
      >
        Create rule from this event
      </Button>
    </>
  );
}
