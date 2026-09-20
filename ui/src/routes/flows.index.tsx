import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { MoreHorizontal, Play, Search, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { ApiError, api, type Flow, type StateType, unwrap } from "@/api/client";
import { FilterSelect } from "@/components/filter-select";
import { upstreamNames } from "@/components/flow-graph";
import { GroupStateRollup } from "@/components/grouped-rows";
import { RescheduleDialog } from "@/components/reschedule-dialog";
import { RunForm } from "@/components/run-form";
import { Tags } from "@/components/run-table";
import { describeSchedule } from "@/components/schedule-editor";
import { lastStates } from "@/components/scope-sidebar";
import { nextSchedule, SkipDialog } from "@/components/skip-dialog";
import { DOT_COLORS, StateBadge } from "@/components/state-badge";
import { BAR_ORDER } from "@/components/state-bar";
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
import { cn, formatFire, relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/flows/")({ component: FlowsPage });

type RecentRun = Flow["recent_runs"][number];

/** Ten bars, newest on the right, coloured by state and sized by duration. */
export function RunSparkline({ runs }: { runs: RecentRun[] }) {
  const ordered = runs.slice().reverse();
  const max = Math.max(1, ...ordered.map((r) => r[3] ?? 0));
  return (
    <span className="inline-flex h-5 items-end gap-0.5" data-testid="run-sparkline">
      {ordered.map(([id, type, name, duration]) => {
        const h = duration == null ? 8 : Math.max(4, Math.round(Math.sqrt(duration / max) * 20));
        return (
          <Link
            key={id}
            to="/runs/$runId"
            params={{ runId: String(id) }}
            title={`${name} (run ${id})`}
            className={cn(
              "block w-1.5 rounded-[1.5px]",
              DOT_COLORS[type as StateType] ?? "bg-muted-foreground",
            )}
            style={{ height: h }}
            data-state={type}
          />
        );
      })}
    </span>
  );
}

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

type Facets = { state: string; schedule: string; tag: string };
const NO_FACETS: Facets = { state: "", schedule: "", tag: "" };

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
      (!facets.state || f.recent_runs[0]?.[1] === facets.state) &&
      (!facets.schedule || (f.schedules.length ? "scheduled" : "unscheduled") === facets.schedule) &&
      (!facets.tag || (f.tags ?? []).includes(facets.tag)) &&
      (!lower || `${f.project}/${f.name}`.toLowerCase().includes(lower)),
  );
  const states = BAR_ORDER.filter((k) => count((f) => f.recent_runs[0]?.[1] === k) > 0);
  const tags = Array.from(new Set(scoped.flatMap((f) => f.tags ?? []))).sort();
  const filtered = !!(facets.state || facets.schedule || facets.tag || lower);

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
              These flows can occupy every engine: {settings.data.saturation_flows.join(", ")}.{" "}
              <Link to="/settings" className="underline">
                Settings
              </Link>
            </div>
          ) : null}
          <div className="flex flex-wrap items-center gap-1.5">
            <FilterSelect
              label="Last run"
              className={FACET}
              value={facets.state}
              anyCount={scoped.length}
              onChange={(state) => setFacets({ ...facets, state })}
              options={states.map((k) => ({
                value: k,
                label: k,
                count: count((f) => f.recent_runs[0]?.[1] === k),
              }))}
            />
            <FilterSelect
              label="Schedule"
              className={FACET}
              value={facets.schedule}
              anyCount={scoped.length}
              onChange={(schedule) => setFacets({ ...facets, schedule })}
              options={[
                { value: "scheduled", label: "Scheduled", count: count((f) => f.schedules.length > 0) },
                { value: "unscheduled", label: "Unscheduled", count: count((f) => f.schedules.length === 0) },
              ]}
            />
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
            <span className="ml-auto text-xs text-muted-foreground" data-testid="flow-count">
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
                  <Th className="w-52">Schedule</Th>
                  <Th className="w-28">Recent runs</Th>
                  <Th className="w-36">Last run</Th>
                  <Th className="w-52">Starts after</Th>
                  <Th className="w-36">Tags</Th>
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

const COLUMNS = 7;
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
              <span className="flex flex-col gap-px">
                <Link
                  to="/flows/$flowId"
                  params={{ flowId: String(f.id) }}
                  className={cn("font-medium hover:underline", f.live && "text-foreground")}
                >
                  {f.name}
                </Link>
                {f.error ? (
                  <span className="text-xs text-red-600">{f.error}</span>
                ) : !f.live ? (
                  <span className="text-xs">
                    Not registered by this server. Last seen {relativeTime(f.last_seen_at)}.
                  </span>
                ) : f.description ? (
                  <span
                    className="max-w-[280px] truncate text-xs text-muted-foreground"
                    title={f.description}
                  >
                    {f.description.split("\n")[0]}
                  </span>
                ) : null}
              </span>
            </Td>
            <Td>
              <span className="flex flex-col gap-px">
                <span>{sched.words}</span>
                {sched.next ? <span className="text-xs text-muted-foreground">{sched.next}</span> : null}
              </span>
            </Td>
            <Td>
              <RunSparkline runs={f.recent_runs} />
            </Td>
            <Td>
              {last ? (
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
              ) : (
                <span className="text-muted-foreground">-</span>
              )}
            </Td>
            <Td>
              <span className="flex items-center gap-1 text-xs" data-testid="starts-after">
                {upstreamNames(f).map((name) => (
                  <UpstreamChip
                    key={name}
                    name={name}
                    flow={all.find((u) => u.project === f.project && u.name === name)}
                  />
                ))}
                {f.batch_key ? (
                  <span className="font-mono text-[11.5px] text-muted-foreground">key={f.batch_key}</span>
                ) : null}
              </span>
            </Td>
            <Td>
              <Tags tags={f.tags} />
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

/** An upstream as a chip with its last-run state, linking to it when it is known. */
function UpstreamChip({ name, flow }: { name: string; flow?: Flow }) {
  const state = flow?.recent_runs[0]?.[1] as StateType | undefined;
  const body = (
    <>
      <span className={cn("size-1.5 rounded-full", state ? DOT_COLORS[state] : "bg-muted-foreground")} />
      {name}
    </>
  );
  const chip =
    "inline-flex h-5 items-center gap-1 rounded-[5px] border bg-card px-1.5 font-medium text-foreground";
  return flow ? (
    <Link
      to="/flows/$flowId"
      params={{ flowId: String(flow.id) }}
      className={cn(chip, "hover:bg-accent hover:no-underline")}
    >
      {body}
    </Link>
  ) : (
    <span className={chip}>{body}</span>
  );
}
