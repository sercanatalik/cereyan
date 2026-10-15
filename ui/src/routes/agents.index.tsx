import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { Play } from "lucide-react";
import { Fragment, useState } from "react";
import { api, type Flow, unwrap } from "@/api/client";
import { RunForm } from "@/components/run-form";
import { RunStrip } from "@/components/run-strip";
import { describeSchedule } from "@/components/schedule-editor";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Modal } from "@/components/ui/modal";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { type Agent, formatCost, formatTokens, toolTags, useAgents, useCatalogue } from "@/lib/agents";
import { useProject } from "@/lib/project";

export const Route = createFileRoute("/agents/")({ component: AgentsPage });

/** Agents by project, then by group, each list in name order. */
function grouped(agents: Agent[]): { project: string; group: string; agents: Agent[] }[] {
  const buckets = new Map<string, Agent[]>();
  for (const a of agents) {
    const key = `${a.project}\u0000${a.definition.group ?? ""}`;
    buckets.set(key, [...(buckets.get(key) ?? []), a]);
  }
  return [...buckets.entries()]
    .map(([key, list]) => {
      const [project, group] = key.split("\u0000");
      return { project, group, agents: list.sort((x, y) => x.name.localeCompare(y.name)) };
    })
    .sort((x, y) => x.project.localeCompare(y.project) || x.group.localeCompare(y.group));
}

function AgentsPage() {
  const navigate = useNavigate();
  const { scope } = useProject();
  const agents = useAgents();
  const catalogue = useCatalogue();
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const [running, setRunning] = useState<Agent | null>(null);
  const flowOf = (a: Agent): Flow | undefined => flows.data?.find((f) => f.id === a.flow_id);
  const start = useMutation({
    mutationFn: async ({ agent, values }: { agent: Agent; values: Record<string, unknown> }) =>
      unwrap(
        await api.POST("/api/flows/{id}/runs", {
          params: { path: { id: agent.flow_id as number } },
          body: { parameters: values as any },
        }),
      ),
    onSuccess: (res) => {
      const run = "conflict" in res ? res.run : res;
      setRunning(null);
      navigate({ to: "/runs/$runId", params: { runId: String(run.id) } });
    },
  });
  const list = (agents.data ?? []).filter((a) => !scope || a.project === scope);
  // A recent run is a tuple: id, state type, state name, duration.
  const waiting = list.filter((a) => flowOf(a)?.recent_runs[0]?.[1] === "Paused").length;
  const version = (catalogue.data?.features as { pydantic_ai?: string } | undefined)?.pydantic_ai;
  return (
    <Page
      title="Agents"
      subtitle={
        agents.data
          ? `${list.length} agent${list.length === 1 ? "" : "s"}${waiting ? ` · ${waiting} waiting for you` : ""}${
              version ? ` · Pydantic AI ${version} on this server` : " · no host has the pydantic-ai extra"
            }`
          : "Loading"
      }
      actions={
        <Button asChild data-testid="new-agent">
          <Link to="/agents/new">New agent</Link>
        </Button>
      }
    >
      {agents.data && list.length === 0 ? (
        <div
          className="rounded-lg border bg-card p-6 text-sm text-muted-foreground"
          data-testid="agents-empty"
        >
          No agents yet. An agent is a flow defined here: a model, instructions, the tools it may use, and a
          policy for approvals and spend. Schedules, rules and backfills then treat it as any flow.
        </div>
      ) : null}
      {list.length ? (
        <div className="overflow-x-auto rounded-lg border bg-card">
          <Table>
            <thead>
              <Tr>
                <Th>Agent</Th>
                <Th>Model</Th>
                <Th>Tools</Th>
                <Th>Schedule</Th>
                <Th>Last 10 runs</Th>
                <Th>Tokens · 24 h</Th>
                <Th />
              </Tr>
            </thead>
            <tbody>
              {grouped(list).map((section) => (
                <Fragment key={`${section.project}/${section.group}`}>
                  <Tr className="bg-muted/40">
                    <Td colSpan={7} className="text-xs text-muted-foreground" data-testid="agent-group">
                      <span className="font-medium text-foreground">{section.project}</span>
                      {section.group ? (
                        <>
                          {" › "}
                          <span className="font-medium text-foreground">{section.group}</span>
                        </>
                      ) : null}
                      {` · ${section.agents.length} agent${section.agents.length === 1 ? "" : "s"}`}
                    </Td>
                  </Tr>
                  {section.agents.map((a) => {
                    const f = flowOf(a);
                    const active = f?.schedules.filter((s) => s.active) ?? [];
                    return (
                      <Tr key={a.id} data-testid="agent-row">
                        <Td>
                          <Link
                            to="/agents/$agentId"
                            params={{ agentId: String(a.id) }}
                            className="font-medium hover:underline"
                            data-testid="agent-name"
                          >
                            {a.name}
                          </Link>
                          {a.source === "code" ? (
                            <span className="ml-1.5 rounded border px-1 text-[10px] text-muted-foreground">
                              code
                            </span>
                          ) : null}
                          {!a.enabled ? (
                            <span className="ml-1.5 rounded border px-1 text-[10px] text-muted-foreground">
                              disabled
                            </span>
                          ) : null}
                          {a.allow_error ? (
                            <span
                              className="ml-1.5 rounded border border-amber-500/60 px-1 text-[10px] text-amber-700 dark:text-amber-400"
                              title={a.allow_error}
                            >
                              not allowed
                            </span>
                          ) : null}
                          <div className="text-xs text-muted-foreground">{a.definition.description}</div>
                        </Td>
                        <Td className="font-mono text-xs">{a.definition.model}</Td>
                        <Td>
                          <div className="flex flex-wrap gap-1">
                            {a.definition.tools.map((t) => (
                              <span
                                key={`${t.kind}:${t.name}`}
                                className="rounded border px-1.5 py-0.5 text-xs"
                              >
                                {t.name}
                                {toolTags(t, a.definition).map((tag) => (
                                  <span key={tag} className="ml-1 text-[10px] text-muted-foreground">
                                    {tag}
                                  </span>
                                ))}
                              </span>
                            ))}
                            {a.definition.tools.length === 0 ? (
                              <span className="text-xs text-muted-foreground">none</span>
                            ) : null}
                          </div>
                        </Td>
                        <Td className="text-sm">
                          {active.length ? (
                            describeSchedule(active[0])
                          ) : (
                            <span className="text-muted-foreground">On demand</span>
                          )}
                        </Td>
                        <Td>{f ? <RunStrip runs={f.recent_runs} /> : null}</Td>
                        <Td className="font-mono text-xs">
                          {formatTokens(a.usage_24h.tokens)}
                          <span className="text-muted-foreground"> · {formatCost(a.usage_24h.cost_usd)}</span>
                        </Td>
                        <Td className="text-right">
                          <Button
                            size="sm"
                            variant="outline"
                            onClick={() => setRunning(a)}
                            disabled={!a.enabled || !!a.allow_error}
                            aria-label={`Run ${a.name}`}
                          >
                            <Play className="size-3.5" /> Run
                          </Button>
                        </Td>
                      </Tr>
                    );
                  })}
                </Fragment>
              ))}
            </tbody>
          </Table>
        </div>
      ) : null}
      <Modal open={!!running} onClose={() => setRunning(null)} title={running ? `Run ${running.name}` : ""}>
        {running ? (
          <RunForm
            schema={running.parameter_schema}
            onSubmit={(values) => start.mutate({ agent: running, values })}
            busy={start.isPending}
            serverError={start.isError ? String(start.error) : null}
          />
        ) : null}
      </Modal>
    </Page>
  );
}
