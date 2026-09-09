import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { MoreHorizontal, Play, Search } from "lucide-react";
import { useState } from "react";
import { ApiError, api, type Flow, type StateType, unwrap } from "@/api/client";
import { FilterSelect } from "@/components/filter-select";
import { FlowGraph } from "@/components/flow-graph";
import {
  GroupCount,
  GroupSection,
  GroupStateRollup,
  GroupTags,
  useGroupOpen,
} from "@/components/grouped-rows";
import { DOT_COLORS, StateBadge } from "@/components/ported/state-badge";
import { RunForm } from "@/components/run-form";
import { Tags } from "@/components/run-table";
import { Page } from "@/components/shell";
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
import { type Group, groupBy, groupOf } from "@/lib/groups";
import { useProject } from "@/lib/project";
import { cn, relativeTime } from "@/lib/utils";
import { describeSchedule } from "./flows.$flowId";

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
  if (nexts.length === 0) return { words, next: active.length ? "" : "paused" };
  return {
    words,
    next: `next ${relativeTime(Math.min(...nexts))
      .replace(" ago", " late")
      .replace(" from now", "")}`,
  };
}

function FlowsPage() {
  const client = useQueryClient();
  const navigate = useNavigate();
  const { project, scope, setProject } = useProject();
  const [q, setQ] = useState("");
  const [target, setTarget] = useState<Flow | null>(null);
  const [error, setError] = useState<string | null>(null);
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: async () => unwrap(await api.GET("/api/settings", {})),
  });
  const projects = Array.from(new Set((flows.data ?? []).map((f) => f.project))).sort();
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
      navigate({ to: "/runs/$runId", params: { runId: String(created.id) } });
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
  const lower = q.trim().toLowerCase();
  const list = (flows.data ?? []).filter(
    (f) =>
      (!project || f.project === project) &&
      (!lower || `${f.project}/${f.name}`.toLowerCase().includes(lower)),
  );
  const groups = groupBy(list);
  const totals = new Map<string, number>();
  // Unfiltered sizes, so a narrowed group's header can read "n of m".
  for (const f of flows.data ?? []) {
    const key = groupOf(f);
    totals.set(key, (totals.get(key) ?? 0) + 1);
  }
  const open = useGroupOpen(groups, lower.length > 0);
  const served = settings.data?.served_dir;
  return (
    <Page
      title="Flows"
      subtitle={
        flows.data ? (
          <>
            {flows.data.length} flows in {projects.length} project{projects.length === 1 ? "" : "s"}
            {served ? (
              <>
                , registered by <span className="font-mono text-xs">cereyan serve {served}</span>
              </>
            ) : null}
          </>
        ) : null
      }
      actions={
        <>
          <div className="relative">
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              placeholder="Search flows"
              aria-label="Search flows"
              value={q}
              onChange={(e) => setQ(e.target.value)}
              className="w-56 pl-8"
            />
          </div>
          <FilterSelect
            label="Project"
            anyLabel="All"
            value={scope}
            onChange={setProject}
            options={projects.map((p) => ({ value: p, label: p }))}
          />
        </>
      }
    >
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
      <FlowGraph flows={list} />
      <Card className="gap-0 overflow-hidden py-0">
        <Table>
          <thead>
            <tr>
              <Th className="w-[300px]">Flow</Th>
              <Th className="w-52">Schedule</Th>
              <Th className="w-28">Recent runs</Th>
              <Th className="w-40">Last run</Th>
              <Th>Tags</Th>
              <Th className="w-32" />
            </tr>
          </thead>
          {groups.map((group) => (
            <GroupSection
              key={group.key}
              group={group}
              open={open.isOpen(group.key)}
              onOpenChange={(next) => open.toggle(group.key, next)}
              identity={group.spansProjects ? group.projects.join(" \u00b7 ") : group.items[0]?.source_dir}
              rollup={<FlowGroupRollup group={group} total={totals.get(group.key)} />}
            >
              <GroupRows
                flows={group.items}
                onRun={(f) => {
                  setTarget(f);
                  setError(null);
                }}
                onDelete={(f) =>
                  window.confirm(`Delete flow ${f.project}/${f.name} and its runs?`) && remove.mutate(f)
                }
              />
            </GroupSection>
          ))}
          {list.length === 0 ? (
            <tbody>
              <tr>
                <td colSpan={6} className="px-3 py-8 text-center text-muted-foreground">
                  {flows.data?.length ? "No flows match" : "No flows registered"}
                </td>
              </tr>
            </tbody>
          ) : null}
        </Table>
      </Card>
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
    </Page>
  );
}

/**
 * The group header's cells, each the rollup of the column beneath it: the
 * soonest fire, the group's runs merged, the last-run states with the flows
 * this server no longer has registered, the tag union, and the flow count.
 */
function FlowGroupRollup({ group, total }: { group: Group<Flow>; total?: number }) {
  const flows = group.items;
  const schedules = flows.flatMap((f) => f.schedules);
  const active = schedules.filter((s) => s.active);
  const nexts = active.map((s) => s.next_fire).filter((n): n is number => n != null);
  const paused = schedules.length - active.length;
  const unscheduled = flows.filter((f) => f.schedules.length === 0).length;
  // Newest first across the group. Run ids are monotonic, so they order the
  // merge without a timestamp, and they identify a run so each appears once.
  const byId = new Map(flows.flatMap((f) => f.recent_runs).map((r) => [r[0], r]));
  const recent = Array.from(byId.values())
    .sort((a, b) => b[0] - a[0])
    .slice(0, 10);
  const counts: Record<string, number> = {};
  for (const f of flows) {
    const last = f.recent_runs[0];
    if (last) counts[last[1]] = (counts[last[1]] ?? 0) + 1;
  }
  const stale = flows.filter((f) => !f.live || f.error).length;
  const tags = Array.from(new Set(flows.flatMap((f) => f.tags ?? []))).sort();
  return (
    <>
      <Td className="text-xs text-muted-foreground">
        <span className="flex flex-col gap-px">
          {nexts.length ? (
            <span>
              next{" "}
              {relativeTime(Math.min(...nexts))
                .replace(" ago", " late")
                .replace(" from now", "")}
            </span>
          ) : null}
          <span>
            {[paused ? `${paused} paused` : null, unscheduled ? `${unscheduled} unscheduled` : null]
              .filter(Boolean)
              .join(", ")}
          </span>
        </span>
      </Td>
      <Td>{recent.length ? <RunSparkline runs={recent} /> : null}</Td>
      <Td>
        <GroupStateRollup counts={counts} stale={stale} />
      </Td>
      <Td>
        <GroupTags tags={tags} />
      </Td>
      <Td className="text-right text-xs">
        <GroupCount shown={flows.length} total={total} />
      </Td>
    </>
  );
}

function GroupRows({
  flows,
  onRun,
  onDelete,
}: {
  flows: Flow[];
  onRun: (f: Flow) => void;
  onDelete: (f: Flow) => void;
}) {
  const navigate = useNavigate();
  return (
    <>
      {flows.map((f) => {
        const sched = scheduleSummary(f);
        const last = f.recent_runs[0];
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
