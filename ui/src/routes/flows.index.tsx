import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { MoreHorizontal, Play, Search, TriangleAlert } from "lucide-react";
import { Fragment, useState } from "react";
import { ApiError, api, type Flow, type StateType, unwrap } from "@/api/client";
import { FilterSelect } from "@/components/filter-select";
import { upstreamNames } from "@/components/flow-graph";
import { GroupStateRollup } from "@/components/grouped-rows";
import { RescheduleDialog } from "@/components/reschedule-dialog";
import { RunForm } from "@/components/run-form";
import { RunStrip, RunStripLegend } from "@/components/run-strip";
import { describeSchedule } from "@/components/schedule-editor";
import { lastStates } from "@/components/scope-sidebar";
import { nextSchedule, SkipDialog } from "@/components/skip-dialog";
import { StateBadge } from "@/components/state-badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Modal } from "@/components/ui/modal";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { groupOf, nestByProject } from "@/lib/groups";
import { useProject } from "@/lib/project";
import {
  flowsAttention,
  healthLine,
  matchesQuickFilter,
  QUICK_FILTERS,
  type QuickFilter,
} from "@/lib/summary/flows";
import { cn, formatDuration, formatFire, relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/flows/")({ component: FlowsPage });

function scheduleSummary(f: Flow): { words: string; next: string } {
  const active = f.schedules.filter((s) => s.active);
  if (f.schedules.length === 0) return { words: "No schedule", next: "" };
  const first = active[0] ?? f.schedules[0];
  const words =
    f.schedules.length > 1
      ? `${describeSchedule(first)} and ${f.schedules.length - 1} more`
      : describeSchedule(first);
  const nexts = active.map((s) => s.next_fire).filter((n): n is number => n != null);
  const skipped = active.reduce((n, s) => n + (s.skipped ?? 0), 0);
  const note = skipped ? `${skipped} skipped` : "";
  if (nexts.length === 0) return { words, next: active.length ? note : "paused" };
  // next_fire is the next fire that will run, so a skipped one never shows here.
  return {
    words,
    next: `next ${relativeTime(Math.min(...nexts))
      .replace(" ago", " late")
      .replace(" from now", "")}${note ? ` · ${note}` : ""}`,
  };
}

type Facets = { chip: QuickFilter; tag: string };
const NO_FACETS: Facets = { chip: "all", tag: "" };

/** The soonest fire across these flows' active schedules, if any. */
function soonestFire(flows: Flow[]): number | null {
  const nexts = flows
    .flatMap((f) => f.schedules)
    .filter((s) => s.active)
    .map((s) => s.next_fire)
    .filter((n): n is number => n != null);
  return nexts.length ? Math.min(...nexts) : null;
}

const inWords = (micros: number) => relativeTime(micros).replace(" ago", " late").replace(" from now", "");

function FlowsPage() {
  const client = useQueryClient();
  const navigate = useNavigate();
  const { project, group, setScope } = useProject();
  const [q, setQ] = useState("");
  const [facets, setFacets] = useState<Facets>(NO_FACETS);
  const [target, setTarget] = useState<Flow | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [skipTarget, setSkipTarget] = useState<Flow | null>(null);
  const [rescheduleTarget, setRescheduleTarget] = useState<Flow | null>(null);
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: async () => unwrap(await api.GET("/api/settings", {})),
  });
  const run = useMutation({
    mutationFn: async ({ flow, body }: { flow: Flow; body: Record<string, unknown> }) =>
      unwrap(
        await api.POST("/api/flows/{id}/runs", {
          params: { path: { id: flow.id } },
          body: { parameters: body as any, tags: [] },
        }),
      ),
    onSuccess: (created) => {
      setTarget(null);
      client.invalidateQueries({ queryKey: ["runs"] });
      navigate({
        to: "/runs/$runId",
        params: { runId: String("conflict" in created ? created.run.id : created.id) },
      });
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
  });
  const remove = useMutation({
    mutationFn: async (flow: Flow) => {
      const result = await api.DELETE("/api/flows/{id}", { params: { path: { id: flow.id } } });
      if (!result.response.ok) throw new ApiError(result.response.status, result.error);
    },
    onSuccess: () => client.invalidateQueries({ queryKey: ["flows"] }),
  });
  const skipNext = useMutation({
    mutationFn: async (flow: Flow) => {
      const schedule = nextSchedule(flow);
      if (!schedule) return;
      unwrap(
        await api.POST("/api/schedules/{sid}/skips", {
          params: { path: { sid: schedule.id } },
          body: { next: 1, by: "ui" },
        }),
      );
    },
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["flows"] });
      client.invalidateQueries({ queryKey: ["upcoming"] });
    },
  });
  const all = flows.data ?? [];
  const tree = nestByProject(all);
  // Facets are questions asked of one scope, so a new scope starts without them.
  const scopeKey = `${project ?? ""}/${group}`;
  const [facetScope, setFacetScope] = useState(scopeKey);
  if (facetScope !== scopeKey) {
    setFacetScope(scopeKey);
    setFacets(NO_FACETS);
  }

  // The scope narrows first; facets and search then narrow within it, and
  // their option counts are counted against the scope, not the narrowed list.
  const scoped = all.filter((f) => (!project || f.project === project) && (!group || groupOf(f) === group));
  const count = (pred: (f: Flow) => boolean) => scoped.filter(pred).length;
  const lower = q.trim().toLowerCase();
  const visible = scoped.filter(
    (f) =>
      matchesQuickFilter(f, facets.chip) &&
      (!facets.tag || (f.tags ?? []).includes(facets.tag)) &&
      (!lower || `${f.project}/${f.name}`.toLowerCase().includes(lower)),
  );
  const tags = Array.from(new Set(scoped.flatMap((f) => f.tags ?? []))).sort();
  const filtered = !!(facets.chip !== "all" || facets.tag || lower);
  const attention = flowsAttention(scoped);

  // One band per group, only while the scope spans more than one group.
  const keyOf = (f: Flow) => `${f.project}/${groupOf(f)}`;
  const banded = new Set(scoped.map(keyOf)).size > 1;
  const sections = new Map<string, Flow[]>();
  for (const f of visible) sections.set(keyOf(f), [...(sections.get(keyOf(f)) ?? []), f]);
  const ordered = Array.from(sections.entries()).sort(([a], [b]) => a.localeCompare(b));

  const current = tree.find((p) => p.key === project);
  const served = settings.data?.served_dir;
  const title = !project
    ? "All flows"
    : !group
      ? project
      : group === project
        ? `${project} · project flows`
        : group;
  const source = current?.items[0]?.source_dir;
  const groupCount = current ? current.groups.length + (current.rows.length ? 1 : 0) : 0;
  const subtitle = !flows.data ? null : !project ? (
    <>
      {all.length} flows in {tree.length} project{tree.length === 1 ? "" : "s"}
      {attention ? ` · ${attention}` : null}
      {served ? (
        <>
          {" "}
          · registered by <span className="font-mono text-xs">cereyan serve {served}</span>
        </>
      ) : null}
    </>
  ) : (
    <>
      {group
        ? `Group in ${project}`
        : `${current?.items.length ?? 0} flows in ${groupCount} group${groupCount === 1 ? "" : "s"}`}
      {attention ? ` · ${attention}` : null}
      {source ? (
        <>
          {" "}
          · <span className="font-mono text-xs">{source}</span>
        </>
      ) : null}
    </>
  );

  return (
    <>
      <div className="flex flex-col" data-testid="flows-main">
        <div className="flex flex-col gap-4 px-(--gutter) pt-[22px] pb-10">
          <div className="flex items-end justify-between gap-4">
            <div className="flex flex-col gap-0.5">
              <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
              {subtitle ? <div className="text-sm text-muted-foreground">{subtitle}</div> : null}
            </div>
            <div className="relative">
              <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                placeholder="Search flows in scope"
                aria-label="Search flows"
                value={q}
                onChange={(e) => setQ(e.target.value)}
                className="w-60 pl-8"
              />
            </div>
          </div>
          {settings.data?.engine_saturation_risk ? (
            <div
              className="rounded-md border border-amber-400 bg-amber-50 px-3 py-2 text-sm text-amber-900 dark:bg-amber-900/30 dark:text-amber-100"
              data-testid="saturation-banner"
            >
              These flows can occupy every processor: {settings.data.saturation_flows.join(", ")}.{" "}
              <Link to="/queue" className="underline">
                Queue
              </Link>
            </div>
          ) : null}
          <div className="flex flex-wrap items-center gap-1.5">
            <fieldset
              className="m-0 flex flex-wrap items-center gap-1.5 border-0 p-0"
              aria-label="Quick filters"
            >
              {QUICK_FILTERS.map((chip) => {
                const on = facets.chip === chip.value;
                return (
                  <button
                    key={chip.value}
                    type="button"
                    aria-pressed={on}
                    data-testid={`chip-${chip.value}`}
                    onClick={() => setFacets({ ...facets, chip: chip.value })}
                    className={cn(
                      "inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-xs font-medium transition-colors",
                      on
                        ? "border-foreground bg-foreground text-background"
                        : "bg-card text-foreground hover:bg-accent",
                    )}
                  >
                    {chip.label}
                    <span
                      className={cn("tabular-nums", on ? "text-background/70" : "text-muted-foreground")}
                      data-testid="chip-count"
                    >
                      {count(chip.match)}
                    </span>
                  </button>
                );
              })}
            </fieldset>
            <FilterSelect
              label="Tags"
              className={FACET}
              value={facets.tag}
              anyCount={scoped.length}
              onChange={(tag) => setFacets({ ...facets, tag })}
              options={tags.map((t) => ({
                value: t,
                label: t,
                count: count((f) => (f.tags ?? []).includes(t)),
              }))}
            />
            {filtered ? (
              <Button
                variant="ghost"
                size="sm"
                className="h-7 px-2 text-xs text-muted-foreground"
                onClick={() => {
                  setQ("");
                  setFacets(NO_FACETS);
                }}
              >
                Clear
              </Button>
            ) : null}
            <RunStripLegend className="ml-auto" />
            <span className="text-xs text-muted-foreground" data-testid="flow-count">
              {visible.length === scoped.length
                ? `${scoped.length} flows`
                : `${visible.length} of ${scoped.length} flows`}
            </span>
          </div>
          {skipNext.error ? (
            <div className="text-xs text-red-600" data-testid="skip-next-error">
              {skipNext.error instanceof ApiError ? skipNext.error.message : String(skipNext.error)}
            </div>
          ) : null}
          {group ? <GroupStats flows={scoped} /> : null}
          <Card className="gap-0 overflow-hidden py-0">
            <Table>
              <thead>
                <tr>
                  <Th>Flow</Th>
                  <Th className="w-56">Schedule</Th>
                  <Th className="w-40">Last 10 runs</Th>
                  <Th className="w-36">Last run</Th>
                  <Th className="w-32" />
                </tr>
              </thead>
              {ordered.map(([key, items]) => {
                const [p, ...rest] = key.split("/");
                const g = rest.join("/");
                const total = scoped.filter((f) => keyOf(f) === key).length;
                const stale = items.filter((f) => !f.live).length;
                return (
                  <tbody key={key}>
                    {banded ? (
                      <tr className="border-b bg-muted/50" data-testid={`section-${key}`}>
                        <td colSpan={COLUMNS} className="h-[30px] px-3">
                          <span className="flex items-center gap-2.5 text-xs">
                            <button
                              type="button"
                              className="inline-flex items-center gap-1.5 font-semibold hover:underline"
                              onClick={() => setScope(p, g)}
                            >
                              {g !== p ? (
                                <span className="font-normal text-muted-foreground">{p} ›</span>
                              ) : null}
                              {g}
                            </button>
                            <span className="text-muted-foreground" data-testid="section-meta">
                              {items.length === total ? total : `${items.length} of ${total}`} flow
                              {total === 1 ? "" : "s"} · {items.filter((f) => f.schedules.length).length}{" "}
                              scheduled
                              {items.some((f) => upstreamNames(f).length) ? " · has dependencies" : ""}
                            </span>
                            {stale ? (
                              <span
                                className="inline-flex items-center gap-1 text-amber-700 dark:text-amber-400"
                                data-testid="section-stale"
                                title={`${stale} not registered by this server`}
                              >
                                <TriangleAlert className="size-3" />
                                {stale} stale
                              </span>
                            ) : null}
                          </span>
                        </td>
                      </tr>
                    ) : null}
                    <GroupRows
                      flows={items}
                      all={all}
                      onRun={(f) => {
                        setTarget(f);
                        setError(null);
                      }}
                      onDelete={(f) =>
                        window.confirm(`Delete flow ${f.project}/${f.name} and its runs?`) && remove.mutate(f)
                      }
                      onSkipNext={(f) => skipNext.mutate(f)}
                      onSkip={setSkipTarget}
                      onReschedule={setRescheduleTarget}
                    />
                  </tbody>
                );
              })}
              {visible.length === 0 ? (
                <tbody>
                  <tr>
                    <td colSpan={COLUMNS} className="px-3 py-8 text-center text-muted-foreground">
                      {!flows.data
                        ? ""
                        : all.length
                          ? "No flows match in this scope."
                          : "No flows registered"}
                    </td>
                  </tr>
                </tbody>
              ) : null}
            </Table>
          </Card>
        </div>
      </div>
      <Modal
        open={target !== null}
        onClose={() => setTarget(null)}
        title={target ? `Run ${target.project}/${target.name}` : ""}
        description="Parameters come from the flow signature."
      >
        {target ? (
          <RunForm
            key={target.id}
            schema={target.parameter_schema}
            onSubmit={(body: Record<string, unknown>) => run.mutate({ flow: target, body })}
            busy={run.isPending}
            serverError={error}
          />
        ) : null}
      </Modal>
      {skipTarget ? (
        <SkipDialog key={skipTarget.id} flow={skipTarget} open onClose={() => setSkipTarget(null)} />
      ) : null}
      {rescheduleTarget ? (
        <RescheduleDialog
          key={rescheduleTarget.id}
          flow={rescheduleTarget}
          open
          onClose={() => setRescheduleTarget(null)}
        />
      ) : null}
    </>
  );
}

const COLUMNS = 5;
const FACET = "h-7 rounded-full pr-2 pl-2.5 text-xs";

/** A group's summary strip: its size, soonest fire, last-run states and dependencies. */
function GroupStats({ flows }: { flows: Flow[] }) {
  const states = lastStates(flows);
  const next = soonestFire(flows);
  const deps = flows
    .filter((f) => upstreamNames(f).length)
    .map((f) => `${upstreamNames(f).join(" + ")} → ${f.name}`);
  const cell = "flex flex-col gap-0.5 bg-card px-4 py-3";
  const label = "text-xs text-muted-foreground";
  return (
    <div
      className="grid grid-cols-4 gap-px overflow-hidden rounded-xl border bg-border shadow-xs"
      data-testid="group-stats"
    >
      <div className={cell}>
        <span className={label}>Flows</span>
        <span className="text-xl font-semibold">{flows.length}</span>
      </div>
      <div className={cell}>
        <span className={label}>Next fire</span>
        <span className="text-xl font-semibold">{next === null ? "—" : inWords(next)}</span>
      </div>
      <div className={cn(cell, "gap-1.5")}>
        <span className={label}>Last runs</span>
        <GroupStateRollup counts={states} stale={flows.filter((f) => !f.live).length} />
      </div>
      <div className={cell}>
        <span className={label}>Dependencies</span>
        <span className="truncate font-mono text-[13px] leading-7" title={deps.join("; ")}>
          {deps.length ? deps.join("; ") : "none"}
        </span>
      </div>
    </div>
  );
}

function GroupRows({
  flows,
  all,
  onRun,
  onDelete,
  onSkipNext,
  onSkip,
  onReschedule,
}: {
  flows: Flow[];
  /** Every flow, to look up the state of an upstream outside the scope. */
  all: Flow[];
  onRun: (f: Flow) => void;
  onDelete: (f: Flow) => void;
  onSkipNext: (f: Flow) => void;
  onSkip: (f: Flow) => void;
  onReschedule: (f: Flow) => void;
}) {
  const navigate = useNavigate();
  return (
    <>
      {flows.map((f) => {
        const sched = scheduleSummary(f);
        const last = f.recent_runs[0];
        const soonest = nextSchedule(f)?.next_fire ?? null;
        return (
          <Tr key={f.id} className={cn(!f.live && "text-muted-foreground")} data-flow-live={f.live}>
            <Td className="h-14">
              <span className="flex min-w-0 flex-col gap-px">
                <Link
                  to="/flows/$flowId"
                  params={{ flowId: String(f.id) }}
                  className={cn("font-medium hover:underline", f.live && "text-foreground")}
                  data-testid="flow-name"
                >
                  {f.name}
                </Link>
                {f.error ? <span className="text-xs text-red-600">{f.error}</span> : null}
                {!f.live ? (
                  <span className="text-xs">
                    Not registered by this server. Last seen {relativeTime(f.last_seen_at)}.
                  </span>
                ) : null}
                <FlowSubLine flow={f} all={all} />
              </span>
            </Td>
            <Td>
              <span className="flex flex-col gap-px">
                <span>{sched.words}</span>
                {sched.next ? <span className="text-xs text-muted-foreground">{sched.next}</span> : null}
              </span>
            </Td>
            <Td>
              <RunStrip runs={f.recent_runs} />
            </Td>
            <Td>
              {last ? (
                <span className="flex flex-col items-start gap-px">
                  <Link to="/runs/$runId" params={{ runId: String(last[0]) }} className="hover:no-underline">
                    <StateBadge
                      state={{
                        type: last[1] as StateType,
                        name: last[2],
                        message: null,
                        details: {},
                        timestamp: 0,
                      }}
                    />
                  </Link>
                  {/* recent_runs carries no timestamps, so the run's duration stands in for its time. */}
                  {last[3] != null ? (
                    <span className="text-xs text-muted-foreground">took {formatDuration(last[3])}</span>
                  ) : null}
                </span>
              ) : (
                <span className="text-muted-foreground">-</span>
              )}
            </Td>
            <Td className="text-right">
              <span className="inline-flex gap-1.5">
                {f.live ? (
                  <Button size="sm" variant="outline" onClick={() => onRun(f)}>
                    <Play /> Run
                  </Button>
                ) : (
                  <Button
                    size="sm"
                    variant="ghost"
                    className="text-muted-foreground"
                    onClick={() => onDelete(f)}
                  >
                    Delete
                  </Button>
                )}
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button size="icon-sm" variant="outline" aria-label={`More actions for ${f.name}`}>
                      <MoreHorizontal />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    {f.schedules.length ? (
                      <>
                        <DropdownMenuItem disabled={soonest === null} onSelect={() => onSkipNext(f)}>
                          Skip next run
                          {soonest !== null ? (
                            <span className="ml-auto pl-4 text-xs text-muted-foreground">
                              {formatFire(soonest)}
                            </span>
                          ) : null}
                        </DropdownMenuItem>
                        <DropdownMenuItem onSelect={() => onSkip(f)}>Skip runs…</DropdownMenuItem>
                        <DropdownMenuItem onSelect={() => onReschedule(f)}>Reschedule…</DropdownMenuItem>
                        <DropdownMenuSeparator />
                      </>
                    ) : null}
                    <DropdownMenuItem
                      onSelect={() => navigate({ to: "/flows/$flowId", params: { flowId: String(f.id) } })}
                    >
                      Open flow
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      onSelect={() =>
                        navigate({
                          to: "/runs",
                          search: { tab: "runs", flow: f.name, project: f.project } as any,
                        })
                      }
                    >
                      Show runs
                    </DropdownMenuItem>
                    {!f.live ? (
                      <>
                        <DropdownMenuSeparator />
                        <DropdownMenuItem variant="destructive" onSelect={() => onDelete(f)}>
                          Delete flow
                        </DropdownMenuItem>
                      </>
                    ) : null}
                  </DropdownMenuContent>
                </DropdownMenu>
              </span>
            </Td>
          </Tr>
        );
      })}
    </>
  );
}

/**
 * The muted line under a flow's name: its description, the flows it runs
 * after, its tags, and its health check in words, the failing one in red.
 */
function FlowSubLine({ flow: f, all }: { flow: Flow; all: Flow[] }) {
  const upstreams = upstreamNames(f);
  const health = healthLine(f.health);
  const parts: React.ReactNode[] = [];
  if (f.description && f.live)
    parts.push(
      <span key="description" className="max-w-[320px] truncate" title={f.description}>
        {f.description.split("\n")[0]}
      </span>,
    );
  if (upstreams.length)
    parts.push(
      <span key="after" data-testid="starts-after">
        after{" "}
        {upstreams.map((name, i) => {
          const up = all.find((u) => u.project === f.project && u.name === name);
          return (
            <span key={name}>
              {i ? ", " : null}
              {up ? (
                <Link
                  to="/flows/$flowId"
                  params={{ flowId: String(up.id) }}
                  className="font-medium text-foreground/80 hover:underline"
                >
                  {name}
                </Link>
              ) : (
                <span className="font-medium">{name}</span>
              )}
            </span>
          );
        })}
        {f.batch_key ? <span className="font-mono text-[11.5px]"> key={f.batch_key}</span> : null}
      </span>,
    );
  if (f.tags?.length)
    parts.push(
      <span key="tags" data-testid="flow-tags">
        {f.tags.join(", ")}
      </span>,
    );
  if (health)
    parts.push(
      <span
        key="health"
        data-testid="flow-health"
        data-failing={health.failing}
        className={cn(health.failing && "font-medium text-red-700 dark:text-red-400")}
      >
        {health.text}
      </span>,
    );
  if (!parts.length) return null;
  return (
    <span className="flex min-w-0 flex-wrap items-center gap-x-1.5 text-xs text-muted-foreground">
      {parts.map((p, i) => (
        <Fragment key={(p as React.ReactElement).key}>
          {i ? <span aria-hidden>·</span> : null}
          {p}
        </Fragment>
      ))}
    </span>
  );
}
