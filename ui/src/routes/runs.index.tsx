import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { Search as SearchIcon, X } from "lucide-react";
import { useState } from "react";
import { api, type StateType, unwrap } from "@/api/client";
import { FilterSelect } from "@/components/filter-select";
import { DateRangeSelect, type RangePreset, rangeStart } from "@/components/ported/date-range";
import { StateBadge } from "@/components/ported/state-badge";
import { TagInput } from "@/components/ported/tag-input";
import { RunTable } from "@/components/run-table";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useProject } from "@/lib/project";
import { formatDuration, formatTime } from "@/lib/utils";

type Search = {
  tab?: "runs" | "tasks";
  state?: string;
  flow?: string;
  project?: string;
  q?: string;
  tag?: string;
};

export const Route = createFileRoute("/runs/")({
  validateSearch: (s: Record<string, unknown>): Search => ({
    tab: s.tab === "tasks" ? "tasks" : "runs",
    state: typeof s.state === "string" ? s.state : undefined,
    flow: typeof s.flow === "string" ? s.flow : undefined,
    project: typeof s.project === "string" ? s.project : undefined,
    q: typeof s.q === "string" ? s.q : undefined,
    tag: typeof s.tag === "string" ? s.tag : undefined,
  }),
  component: RunsPage,
});

const STATE_TYPES: StateType[] = [
  "Scheduled",
  "Pending",
  "Running",
  "Completed",
  "Failed",
  "Cancelled",
  "Crashed",
  "Paused",
  "Cancelling",
];

const SORTS = [
  { value: "created_desc", label: "Newest first" },
  { value: "created_asc", label: "Oldest first" },
  { value: "start_desc", label: "Latest start" },
  { value: "duration_desc", label: "Longest" },
  { value: "duration_asc", label: "Shortest" },
  { value: "name_asc", label: "Name" },
];

const PAGE = 50;

function RunsPage() {
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const set = (patch: Partial<Search>) => navigate({ search: (old) => ({ ...old, ...patch }) });
  const [range, setRange] = useState<RangePreset>("7d");
  const [tags, setTags] = useState<string[]>(search.tag ? [search.tag] : []);
  const [sort, setSort] = useState("created_desc");
  const { project } = useProject(search.project);
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const projects = Array.from(new Set((flows.data ?? []).map((f) => f.project))).sort();
  const tab = search.tab ?? "runs";
  return (
    <Page
      title="Runs"
      actions={
        <Tabs value={tab} onValueChange={(v) => set({ tab: v as Search["tab"] })}>
          <TabsList>
            <TabsTrigger value="runs">Runs</TabsTrigger>
            <TabsTrigger value="tasks">Task runs</TabsTrigger>
          </TabsList>
        </Tabs>
      }
    >
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative">
          <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder="Search by name"
            value={search.q ?? ""}
            onChange={(e) => set({ q: e.target.value || undefined })}
            className="w-60 pl-8"
            aria-label="Search by name"
          />
        </div>
        <FilterSelect
          label="State"
          value={search.state ?? ""}
          onChange={(v) => set({ state: v || undefined })}
          options={STATE_TYPES.map((s) => ({ value: s, label: s }))}
        />
        <FilterSelect
          label="Project"
          anyLabel="All"
          value={search.project ?? ""}
          onChange={(v) => set({ project: v || undefined, flow: undefined })}
          options={projects.map((p) => ({ value: p, label: p }))}
        />
        <FilterSelect
          label="Flow"
          anyLabel="All"
          searchable
          value={search.flow ?? ""}
          onChange={(v) => set({ flow: v || undefined })}
          options={(flows.data ?? [])
            .filter((f) => !project || f.project === project)
            .map((f) => ({ value: f.name, label: `${f.project}/${f.name}` }))}
        />
        <TagInput value={tags} onChange={setTags} placeholder="Tags" />
        <DateRangeSelect value={range} onChange={setRange} />
        {tab === "runs" ? (
          <FilterSelect
            label="Sort"
            className="ml-auto"
            value={sort}
            allowAny={false}
            onChange={setSort}
            options={SORTS}
            aria-label="Sort"
          />
        ) : null}
      </div>
      {tab === "runs" ? (
        <RunsTab search={search} project={project} tags={tags} start={rangeStart(range)} sort={sort} />
      ) : (
        <TaskRunsTab search={search} project={project} start={rangeStart(range)} />
      )}
    </Page>
  );
}

function PageFooter({
  from,
  count,
  noun,
  hasPrev,
  hasNext,
  onPrev,
  onNext,
}: {
  from: number;
  count: number;
  noun: string;
  hasPrev: boolean;
  hasNext: boolean;
  onPrev: () => void;
  onNext: () => void;
}) {
  return (
    <div className="flex items-center justify-between border-t px-4 py-2.5">
      <span className="text-xs tabular-nums text-muted-foreground">
        {count === 0 ? `No ${noun}` : `${from}–${from + count - 1}${hasNext ? " of more" : ""} ${noun}`}
      </span>
      <div className="flex gap-1.5">
        <Button variant="outline" size="sm" disabled={!hasPrev} onClick={onPrev}>
          Previous
        </Button>
        <Button variant="outline" size="sm" disabled={!hasNext} onClick={onNext}>
          Next
        </Button>
      </div>
    </div>
  );
}

function RunsTab({
  search,
  project,
  tags,
  start,
  sort,
}: {
  search: Search;
  project?: string;
  tags: string[];
  start?: number;
  sort: string;
}) {
  const client = useQueryClient();
  const [cursor, setCursor] = useState<number | undefined>();
  const [history, setHistory] = useState<number[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const query = useQuery({
    queryKey: ["runs", "list", search.state, search.flow, project, search.q, tags, start, sort, cursor],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/runs", {
          params: {
            query: {
              state_type: search.state,
              flow: search.flow,
              project,
              name: search.q,
              tags: tags.join(",") || undefined,
              start_after: start,
              sort,
              limit: PAGE,
              cursor,
            },
          },
        }),
      ),
  });
  const bulk = useMutation({
    mutationFn: async (action: "cancel" | "delete") => {
      for (const id of selected) {
        if (action === "cancel") await api.POST("/api/runs/{id}/cancel", { params: { path: { id } } });
        else await api.DELETE("/api/runs/{id}", { params: { path: { id } } });
      }
    },
    onSuccess: () => {
      setSelected(new Set());
      client.invalidateQueries({ queryKey: ["runs"] });
    },
  });
  const runs = query.data?.items ?? [];
  return (
    <>
      <Card className="gap-0 overflow-hidden py-0">
        <RunTable
          runs={runs}
          grouped
          searchActive={!!search.q}
          selected={selected}
          onSelect={(id, checked) =>
            setSelected((old) => {
              const next = new Set(old);
              if (checked) next.add(id);
              else next.delete(id);
              return next;
            })
          }
          onSelectAll={(checked) => setSelected(checked ? new Set(runs.map((r) => r.id)) : new Set())}
        />
        <PageFooter
          from={history.length * PAGE + 1}
          count={runs.length}
          noun="runs"
          hasPrev={history.length > 0}
          hasNext={!!query.data?.next_cursor}
          onPrev={() => {
            const prev = history.slice();
            const c = prev.pop();
            setHistory(prev);
            setCursor(c || undefined);
          }}
          onNext={() => {
            setHistory((h) => [...h, cursor ?? 0]);
            setCursor(query.data?.next_cursor ?? undefined);
          }}
        />
      </Card>
      {selected.size ? (
        <div
          className="fixed bottom-6 left-1/2 z-40 flex -translate-x-1/2 items-center gap-1.5 rounded-[10px] bg-primary py-1.5 pr-1.5 pl-3.5 text-primary-foreground shadow-[0_8px_24px_rgba(28,25,23,0.22)]"
          role="toolbar"
          aria-label="Selection"
          data-testid="selection-bar"
        >
          <span className="mr-2 text-sm font-medium tabular-nums">{selected.size} selected</span>
          <Button
            size="sm"
            className="bg-primary-foreground/15 text-primary-foreground hover:bg-primary-foreground/25"
            onClick={() => bulk.mutate("cancel")}
            disabled={bulk.isPending}
          >
            Cancel
          </Button>
          <Button
            size="sm"
            variant="destructive"
            onClick={() => window.confirm(`Delete ${selected.size} run(s)?`) && bulk.mutate("delete")}
            disabled={bulk.isPending}
          >
            Delete
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            className="text-primary-foreground/70 hover:bg-primary-foreground/15 hover:text-primary-foreground"
            aria-label="Clear selection"
            onClick={() => setSelected(new Set())}
          >
            <X />
          </Button>
        </div>
      ) : null}
    </>
  );
}

function TaskRunsTab({ search, project, start }: { search: Search; project?: string; start?: number }) {
  const [cursor, setCursor] = useState<number | undefined>();
  const [history, setHistory] = useState<number[]>([]);
  const query = useQuery({
    queryKey: ["task-runs", search.state, search.flow, project, search.q, start, cursor],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/task-runs", {
          params: {
            query: {
              state_type: search.state,
              flow: search.flow,
              project,
              name: search.q,
              start_after: start,
              limit: PAGE,
              cursor,
            },
          },
        }),
      ),
  });
  const items = query.data?.items ?? [];
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <Table>
        <thead>
          <tr>
            <Th>State</Th>
            <Th>Task</Th>
            <Th>Run</Th>
            <Th>Flow</Th>
            <Th>Started</Th>
            <Th>Duration</Th>
          </tr>
        </thead>
        <tbody>
          {items.map((t) => (
            <Tr key={t.id}>
              <Td>
                <StateBadge state={t.state} />
              </Td>
              <Td>
                <Link
                  to="/task-runs/$taskRunId"
                  params={{ taskRunId: String(t.id) }}
                  className="font-medium hover:underline"
                >
                  {t.dynamic_key}
                </Link>
              </Td>
              <Td>
                <Link to="/runs/$runId" params={{ runId: String(t.run_id) }} className="hover:underline">
                  {t.run_name}
                </Link>
              </Td>
              <Td className="text-muted-foreground">
                {t.project}/{t.flow_name}
              </Td>
              <Td className="tabular-nums">{formatTime(t.start_time ?? t.created_at)}</Td>
              <Td className="tabular-nums">{formatDuration(t.total_run_time)}</Td>
            </Tr>
          ))}
          {items.length === 0 ? (
            <tr>
              <td colSpan={6} className="px-3 py-8 text-center text-muted-foreground">
                No task runs
              </td>
            </tr>
          ) : null}
        </tbody>
      </Table>
      <PageFooter
        from={history.length * PAGE + 1}
        count={items.length}
        noun="task runs"
        hasPrev={history.length > 0}
        hasNext={!!query.data?.next_cursor}
        onPrev={() => {
          const prev = history.slice();
          const c = prev.pop();
          setHistory(prev);
          setCursor(c || undefined);
        }}
        onNext={() => {
          setHistory((h) => [...h, cursor ?? 0]);
          setCursor(query.data?.next_cursor ?? undefined);
        }}
      />
    </Card>
  );
}
