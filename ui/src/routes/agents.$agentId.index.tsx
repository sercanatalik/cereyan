import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { api, unwrap } from "@/api/client";
import { AgentHandoffs } from "@/components/agent-handoffs";
import { KeyValueList } from "@/components/key-value-list";
import { RunForm } from "@/components/run-form";
import { describeSchedule } from "@/components/schedule-editor";
import { Page } from "@/components/shell";
import { StateBadge } from "@/components/state-badge";
import { Button } from "@/components/ui/button";
import { CardHead } from "@/components/ui/card";
import { Modal } from "@/components/ui/modal";
import { Switch } from "@/components/ui/switch";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { UnderlineTabs } from "@/components/ui/underline-tabs";
import { formatCost, formatTokens, toolTags, useAgent } from "@/lib/agents";
import { handoffsOf } from "@/lib/handoffs";
import { formatDuration, formatTime } from "@/lib/utils";

export const Route = createFileRoute("/agents/$agentId/")({ component: AgentPage });

function AgentPage() {
  const { agentId } = Route.useParams();
  const id = Number(agentId);
  const navigate = useNavigate();
  const client = useQueryClient();
  const agent = useAgent(id);
  const a = agent.data;
  const flow = useQuery({
    queryKey: ["flow", a?.flow_id],
    queryFn: async () =>
      unwrap(await api.GET("/api/flows/{id}", { params: { path: { id: a?.flow_id as number } } })),
    enabled: a?.flow_id != null,
  });
  const runs = useQuery({
    queryKey: ["agent-runs", id, a?.project, a?.name],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/runs", {
          params: { query: { project: a?.project, flow: a?.name, limit: 20 } as any },
        }),
      ),
    enabled: !!a,
  });
  const approvals = useQuery({
    queryKey: ["agent-approvals", id],
    queryFn: async () => unwrap(await api.GET("/api/agents/{id}/approvals", { params: { path: { id } } })),
  });
  const [running, setRunning] = useState(false);
  const [tab, setTab] = useState("overview");
  const rules = useQuery({ queryKey: ["rules"], queryFn: async () => unwrap(await api.GET("/api/rules")) });
  const invalidate = () => {
    client.invalidateQueries({ queryKey: ["agent", id] });
    client.invalidateQueries({ queryKey: ["agents"] });
  };
  const toggle = useMutation({
    mutationFn: async (enabled: boolean) =>
      unwrap(
        await api.PATCH("/api/agents/{id}", {
          params: { path: { id } },
          body: { ...(a?.definition as any), enabled } as any,
        }),
      ),
    onSuccess: invalidate,
  });
  const remove = useMutation({
    mutationFn: async () => api.DELETE("/api/agents/{id}", { params: { path: { id } } }),
    onSuccess: () => {
      invalidate();
      navigate({ to: "/agents" });
    },
  });
  const revoke = useMutation({
    mutationFn: async (aid: number) =>
      api.DELETE("/api/agents/{id}/approvals/{aid}", { params: { path: { id, aid } } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["agent-approvals", id] }),
  });
  const start = useMutation({
    mutationFn: async (values: Record<string, unknown>) =>
      unwrap(
        await api.POST("/api/flows/{id}/runs", {
          params: { path: { id: a?.flow_id as number } },
          body: { parameters: values as any },
        }),
      ),
    onSuccess: (res) => {
      const run = "conflict" in res ? res.run : res;
      setRunning(false);
      navigate({ to: "/runs/$runId", params: { runId: String(run.id) } });
    },
  });
  if (!a) return <Page crumbs={[{ label: "Agents", to: "/agents" }, { label: agentId }]}>Loading</Page>;
  const d = a.definition;
  const last = runs.data?.items[0];
  const links = handoffsOf(rules.data ?? [], a.name);
  const handoffCount = links.incoming.length + links.outgoing.length;
  return (
    <Page crumbs={[{ label: "Agents", to: "/agents" }, { label: a.name }]}>
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-xl font-semibold tracking-tight">{a.name}</h1>
        {last ? <StateBadge state={last.state} /> : null}
        <span className="rounded border px-1 text-[10px] text-muted-foreground">
          {a.source === "ui" ? "defined in the UI" : "code"}
        </span>
        <label htmlFor="agent-enabled" className="flex items-center gap-2 text-sm">
          <Switch
            id="agent-enabled"
            checked={a.enabled}
            onCheckedChange={(v) => toggle.mutate(v)}
            aria-label="Enabled"
          />
          {a.enabled ? "Enabled" : "Disabled"}
        </label>
        <div className="ml-auto flex items-center gap-2">
          <Button variant="outline" size="sm" asChild>
            <Link to="/agents/$agentId/edit" params={{ agentId }}>
              Edit
            </Link>
          </Button>
          {a.flow_id != null ? (
            <Button variant="outline" size="sm" asChild>
              <Link to="/flows/$flowId" params={{ flowId: String(a.flow_id) }}>
                Schedules…
              </Link>
            </Button>
          ) : null}
          <Button
            size="sm"
            onClick={() => setRunning(true)}
            disabled={!a.enabled || !!a.allow_error}
            data-testid="run-agent"
          >
            Run…
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              if (window.confirm(`Delete agent ${a.name}? Its runs stay as history.`)) remove.mutate();
            }}
          >
            Delete
          </Button>
        </div>
      </div>
      <div className="text-sm text-muted-foreground">{d.description}</div>
      {a.allow_error ? (
        <div
          className="rounded-md border border-amber-500/60 bg-amber-50 p-3 text-sm dark:bg-amber-950/30"
          role="alert"
          data-testid="allow-error"
        >
          Not allowed to run: {a.allow_error}
        </div>
      ) : null}

      <UnderlineTabs
        items={[
          { value: "overview", label: "Overview" },
          { value: "handoffs", label: handoffCount ? `Handoffs ${handoffCount}` : "Handoffs" },
        ]}
        value={tab}
        onChange={setTab}
      />
      {tab === "handoffs" ? (
        <AgentHandoffs flowName={a.name} project={a.project} latestRunId={last?.id} />
      ) : null}
      <div className={tab === "overview" ? "grid grid-cols-1 gap-4 lg:grid-cols-3" : "hidden"}>
        <div className="flex flex-col gap-4">
          <section className="rounded-lg border bg-card" aria-label="Definition">
            <CardHead title="Definition" />
            <div className="p-4">
              <KeyValueList
                items={[
                  {
                    label: "Model",
                    value: (
                      <code className="font-mono text-xs">
                        {d.model}
                        {d.api_key ? ` · key ${d.api_key}` : ""}
                      </code>
                    ),
                  },
                  { label: "Instructions", value: <span className="text-xs">{d.instructions || "—"}</span> },
                  { label: "Prompt", value: <code className="font-mono text-xs">{d.prompt || "—"}</code> },
                  {
                    label: "Parameters",
                    value: (
                      <span className="font-mono text-xs">
                        {Object.keys((a.parameter_schema as any)?.properties ?? {}).join(", ")}
                      </span>
                    ),
                  },
                  {
                    label: "Tools",
                    value: (
                      <div className="flex flex-wrap gap-1">
                        {d.tools.map((t) => (
                          <span key={`${t.kind}:${t.name}`} className="rounded border px-1.5 py-0.5 text-xs">
                            {t.name}
                            {toolTags(t, d).map((tag) => (
                              <span key={tag} className="ml-1 text-[10px] text-muted-foreground">
                                {tag}
                              </span>
                            ))}
                          </span>
                        ))}
                        {d.tools.length === 0 ? (
                          <span className="text-xs text-muted-foreground">none</span>
                        ) : null}
                      </div>
                    ),
                  },
                  {
                    label: "Approvals",
                    value: d.policy.approvals === "ask" ? "Ask in the inbox" : d.policy.approvals,
                  },
                  {
                    label: "Limits",
                    value: (
                      <code className="font-mono text-xs">
                        {[
                          d.limits.tokens != null ? `${d.limits.tokens} tokens` : null,
                          d.limits.requests != null ? `${d.limits.requests} requests` : null,
                          d.limits.cost_usd != null ? `$${d.limits.cost_usd}` : null,
                          d.limits.timeout_seconds != null ? `${d.limits.timeout_seconds} s` : null,
                          `${d.crash_retries} crash retries`,
                        ]
                          .filter(Boolean)
                          .join(" · ")}
                      </code>
                    ),
                  },
                  { label: "Output", value: d.output.kind === "json" ? "JSON matching a schema" : "Text" },
                ]}
              />
            </div>
          </section>
          <section className="rounded-lg border bg-card" aria-label="Schedule">
            <CardHead title="Schedule" />
            <div className="p-4 text-sm">
              {flow.data?.schedules.length ? (
                flow.data.schedules.map((s) => (
                  <div key={s.id}>
                    {describeSchedule(s)}
                    {!s.active ? <span className="text-muted-foreground"> · paused</span> : null}
                  </div>
                ))
              ) : (
                <span className="text-muted-foreground">No schedule. Add one on the flow page.</span>
              )}
            </div>
          </section>
          <section className="rounded-lg border bg-card" aria-label="Spend">
            <CardHead title="Spend" />
            <div className="flex flex-col gap-1 p-4 text-sm">
              <div>
                <span className="text-xl font-semibold">{formatCost(a.usage_24h.cost_usd)}</span>
                <span className="text-muted-foreground">
                  {" "}
                  · {formatTokens(a.usage_24h.tokens)} tokens · {a.usage_24h.runs} runs in 24 h
                </span>
              </div>
              <div className="text-xs text-muted-foreground" data-testid="cost-today">
                All agents today: {formatCost(a.cost_today)}
                {a.cost_cap ? ` of ${formatCost(a.cost_cap)} cap` : ", no daily cap"}
              </div>
            </div>
          </section>
        </div>

        <div className="flex flex-col gap-4 lg:col-span-2">
          <section className="rounded-lg border bg-card" aria-label="Runs">
            <CardHead title="Runs" aside={runs.data ? `${runs.data.items.length} most recent` : undefined} />
            <Table>
              <thead>
                <Tr>
                  <Th>State</Th>
                  <Th>Run</Th>
                  <Th>Started by</Th>
                  <Th>Outcome</Th>
                  <Th>Tokens</Th>
                  <Th>When</Th>
                </Tr>
              </thead>
              <tbody>
                {(runs.data?.items ?? []).map((r) => {
                  const attrs = (r.attributes ?? {}) as {
                    outcome?: string;
                    tokens?: number;
                    cost_usd?: number;
                  };
                  return (
                    <Tr key={r.id} data-testid="agent-run">
                      <Td>
                        <StateBadge state={r.state} />
                      </Td>
                      <Td>
                        <Link to="/runs/$runId" params={{ runId: String(r.id) }} className="hover:underline">
                          {r.name}
                        </Link>
                      </Td>
                      <Td className="text-xs text-muted-foreground">{r.created_by}</Td>
                      <Td
                        className="max-w-[280px] truncate font-mono text-xs text-muted-foreground"
                        title={attrs.outcome}
                      >
                        {attrs.outcome ?? (r.state.type === "Failed" ? r.state.message : "")}
                      </Td>
                      <Td className="font-mono text-xs">
                        {attrs.tokens != null ? formatTokens(attrs.tokens) : ""}
                      </Td>
                      <Td className="text-xs text-muted-foreground">
                        {formatTime(r.created_at)}
                        {r.total_run_time != null ? ` · ${formatDuration(r.total_run_time)}` : ""}
                      </Td>
                    </Tr>
                  );
                })}
                {runs.data && runs.data.items.length === 0 ? (
                  <Tr>
                    <Td colSpan={6} className="text-sm text-muted-foreground">
                      No runs yet.
                    </Td>
                  </Tr>
                ) : null}
              </tbody>
            </Table>
          </section>
          <section className="rounded-lg border bg-card" aria-label="Remembered approvals">
            <CardHead
              title="Always allowed"
              aside="tool calls approved with “always allow”; revoke to be asked again"
            />
            <div className="flex flex-col gap-2 p-4 text-sm">
              {(approvals.data ?? []).map((ap) => (
                <div key={ap.id} className="flex items-center gap-3" data-testid="approval">
                  <code className="font-mono text-xs">
                    {ap.tool}({JSON.stringify(ap.args)})
                  </code>
                  <span className="text-xs text-muted-foreground">since {formatTime(ap.created_at)}</span>
                  <Button
                    variant="outline"
                    size="sm"
                    className="ml-auto"
                    onClick={() => revoke.mutate(ap.id)}
                  >
                    Revoke
                  </Button>
                </div>
              ))}
              {approvals.data && approvals.data.length === 0 ? (
                <span className="text-muted-foreground">Nothing remembered.</span>
              ) : null}
            </div>
          </section>
        </div>
      </div>
      <Modal open={running} onClose={() => setRunning(false)} title={`Run ${a.name}`}>
        <RunForm
          schema={a.parameter_schema}
          onSubmit={(values) => start.mutate(values)}
          busy={start.isPending}
          serverError={start.isError ? String(start.error) : null}
        />
      </Modal>
    </Page>
  );
}
