import { useQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { DateRangeSelect, type RangePreset, rangeStart } from "@/components/ported/date-range";
import { JsonView } from "@/components/ported/json-view";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/input";
import { useLiveEvent } from "@/lib/live";
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

function EventsPage() {
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const nav = useNavigate();
  const set = (patch: Partial<Search>) => navigate({ search: (old) => ({ ...old, ...patch }) });
  const [range, setRange] = useState<RangePreset>("24h");
  const [selected, setSelected] = useState<Event | null>(null);
  const [live, setLive] = useState<Event[]>([]);
  const [scrollTop, setScrollTop] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const { project } = useProject();
  const scopedFlows = (flows.data ?? []).filter((f) => !project || f.project === project);
  const scopedIds = project ? new Set(scopedFlows.map((f) => f.id)) : null;
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
  const items = [...live, ...(query.data?.items ?? [])].filter(
    (e) => !scopedIds || e.flow_id == null || scopedIds.has(e.flow_id),
  );
  const viewport = 560;
  const first = Math.max(0, Math.floor(scrollTop / ROW) - 5);
  const last = Math.min(items.length, Math.ceil((scrollTop + viewport) / ROW) + 5);
  return (
    <Page crumbs={[{ label: "Events" }]} title="Events">
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
        <span className="text-xs text-muted-foreground">{items.length} events</span>
      </div>
      <div className="flex gap-3">
        <div
          ref={listRef}
          className="min-w-0 flex-1 overflow-auto rounded-md border bg-card"
          style={{ maxHeight: viewport }}
          onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
          data-testid="event-feed"
        >
          <div style={{ height: items.length * ROW, position: "relative" }}>
            {items.slice(first, last).map((e, i) => (
              <button
                type="button"
                key={e.id}
                onClick={() => setSelected(e)}
                className={cn(
                  "absolute left-0 flex w-full items-center gap-3 border-b px-3 text-left text-sm hover:bg-accent/50",
                  selected?.id === e.id && "bg-accent",
                )}
                style={{ top: (first + i) * ROW, height: ROW }}
              >
                <span className="w-40 shrink-0 text-xs text-muted-foreground">{formatTime(e.occurred)}</span>
                <span className="w-44 shrink-0 font-mono text-xs">{e.name}</span>
                <span className="truncate text-muted-foreground">
                  {e.resource.kind} {e.resource.name}
                </span>
              </button>
            ))}
            {items.length === 0 ? <div className="p-6 text-muted-foreground">No events</div> : null}
          </div>
        </div>
        {selected ? (
          <aside className="w-96 shrink-0 space-y-3 rounded-md border bg-card p-3" data-testid="event-drawer">
            <div className="font-mono text-sm">{selected.name}</div>
            <div className="text-xs text-muted-foreground">
              {formatTime(selected.occurred)} · seq {selected.seq} · {selected.resource.kind}{" "}
              {selected.resource.name}
            </div>
            <JsonView value={selected.payload} />
            <Button
              size="sm"
              onClick={() =>
                nav({
                  to: "/rules",
                  search: {
                    new: "1",
                    events: selected.name,
                    flow: selected.related?.find((r) => r.kind === "flow")?.name ?? "",
                  } as any,
                })
              }
            >
              Create rule from this event
            </Button>
          </aside>
        ) : null}
      </div>
    </Page>
  );
}
