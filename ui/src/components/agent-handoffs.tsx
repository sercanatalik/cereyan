import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { api, unwrap } from "@/api/client";
import { HandoffEditor, type Target } from "@/components/handoff-editor";
import { StateBadge } from "@/components/state-badge";
import { Button } from "@/components/ui/button";
import { CardHead } from "@/components/ui/card";
import { Modal } from "@/components/ui/modal";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { formatTokens, useAgents } from "@/lib/agents";
import {
  chainRows,
  describeWhen,
  emptyHandoff,
  formOf,
  type HandoffForm,
  handoffsOf,
  type Rule,
  reachesBack,
  roundsUsed,
  ruleOf,
  runFlowActions,
  whenOf,
} from "@/lib/handoffs";
import { formatDuration, formatTime } from "@/lib/utils";

/** The Handoffs tab of an agent: where its work comes from and goes, each link
 * a rule, and the last chain its runs were part of. */
export function AgentHandoffs({
  flowName,
  project,
  latestRunId,
}: {
  flowName: string;
  project: string;
  latestRunId?: number;
}) {
  const client = useQueryClient();
  const rules = useQuery({ queryKey: ["rules"], queryFn: async () => unwrap(await api.GET("/api/rules")) });
  const flows = useQuery({ queryKey: ["flows"], queryFn: async () => unwrap(await api.GET("/api/flows")) });
  const agents = useAgents();
  const chain = useQuery({
    queryKey: ["chain", latestRunId],
    queryFn: async () =>
      unwrap(await api.GET("/api/runs/{id}/chain", { params: { path: { id: latestRunId as number } } })),
    enabled: latestRunId != null,
  });
  const [editing, setEditing] = useState<{ rule?: Rule; form: HandoffForm } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const invalidate = () => client.invalidateQueries({ queryKey: ["rules"] });
  const save = useMutation({
    mutationFn: async ({ rule, body }: { rule?: Rule; body: ReturnType<typeof ruleOf> }) =>
      rule
        ? unwrap(await api.PATCH("/api/rules/{id}", { params: { path: { id: rule.id } }, body: body as any }))
        : unwrap(await api.POST("/api/rules", { body })),
    onSuccess: () => {
      setEditing(null);
      setError(null);
      invalidate();
    },
    onError: (e) => setError(String(e)),
  });
  const remove = useMutation({
    mutationFn: async (rule: Rule) => api.DELETE("/api/rules/{id}", { params: { path: { id: rule.id } } }),
    onSuccess: invalidate,
  });
  const all = rules.data ?? [];
  const { outgoing, incoming } = handoffsOf(all, flowName);
  const agentNames = new Set((agents.data ?? []).map((a) => a.name));
  const targets: Target[] = (flows.data ?? [])
    .filter((f) => f.project === project)
    .map((f) => ({
      name: f.name,
      kind: agentNames.has(f.name) ? ("agent" as const) : ("flow" as const),
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
  const kindOf = (name: string) => (agentNames.has(name) ? "agent" : "flow");
  const runs = chain.data?.runs;
  const describe = (rule: Rule) => {
    const action = runFlowActions(rule)[0];
    const loop = action?.flow ? reachesBack(all, rule.when?.flows?.[0] ?? "", action.flow, rule.id) : false;
    return { action, loop };
  };
  return (
    <div className="flex flex-col gap-4" data-testid="agent-handoffs">
      <section className="rounded-lg border bg-card" aria-label="Where this agent's work goes">
        <CardHead
          title="Where this agent's work goes"
          aside="a handoff is a rule: the next agent or flow starts when this one ends, with the result in its prompt"
        />
        <div className="flex flex-col gap-3 p-4 text-sm">
          {incoming.map((rule) => {
            const from = rule.when?.flows?.[0] ?? "any flow";
            return (
              <div key={rule.id} className="flex flex-wrap items-center gap-2" data-testid="handoff-in">
                <span className="rounded border px-1.5 py-0.5 font-mono text-xs">{from}</span>
                <span className="text-xs text-muted-foreground">{kindOf(from)}</span>
                <span className="text-muted-foreground">→ {describeWhen(whenOf(rule))}</span>
                {rule.condition ? <code className="text-xs">if {rule.condition}</code> : null}
                <span className="text-muted-foreground">→</span>
                <span className="font-medium">{flowName}</span>
                <Link to="/rules" className="ml-auto text-xs underline">
                  rule · {rule.name}
                </Link>
              </div>
            );
          })}
          {outgoing.map((rule) => {
            const { action, loop } = describe(rule);
            const used = roundsUsed(runs, rule.id);
            return (
              <div key={rule.id} className="flex flex-wrap items-center gap-2" data-testid="handoff-out">
                <span className="font-medium">{flowName}</span>
                <span className="text-muted-foreground">→ {describeWhen(whenOf(rule))}</span>
                {rule.condition ? <code className="text-xs">if {rule.condition}</code> : null}
                <span className="text-muted-foreground">→</span>
                <span className="rounded border px-1.5 py-0.5 font-mono text-xs">{action?.flow}</span>
                <span className="text-xs text-muted-foreground">{kindOf(action?.flow ?? "")}</span>
                {loop ? (
                  <span className="rounded bg-purple-100 px-1 text-[10px] text-purple-800 dark:bg-purple-950 dark:text-purple-300">
                    ↩ loop · at most {rule.max_per_chain}
                    {action?.delay ? ` · ${action.delay} s between rounds` : ""}
                    {runs ? ` · ${used} of ${rule.max_per_chain} in the last chain` : ""}
                  </span>
                ) : null}
                <span className="ml-auto flex gap-1">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      setError(null);
                      setEditing({ rule, form: formOf(rule, flowName) });
                    }}
                  >
                    Edit
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => remove.mutate(rule)}>
                    Remove
                  </Button>
                </span>
              </div>
            );
          })}
          {rules.data && incoming.length === 0 && outgoing.length === 0 ? (
            <span className="text-muted-foreground" data-testid="handoffs-empty">
              No handoffs yet. Add one to run another agent or flow when this one ends.
            </span>
          ) : null}
          <div>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                setError(null);
                setEditing({ form: emptyHandoff(flowName) });
              }}
              data-testid="add-handoff"
            >
              + Add handoff
            </Button>
          </div>
        </div>
      </section>

      <section className="rounded-lg border bg-card" aria-label="Last chain">
        <CardHead
          title="Last chain"
          aside={
            runs
              ? `${runs.length} run${runs.length === 1 ? "" : "s"} · ${formatTokens(
                  runs.reduce((n, r) => n + Number((r.attributes as { tokens?: number })?.tokens ?? 0), 0),
                )} tokens`
              : undefined
          }
        />
        {runs && runs.length > 1 ? (
          <Table>
            <thead>
              <Tr>
                <Th>Run</Th>
                <Th>Result handed on</Th>
                <Th>Tokens</Th>
                <Th>Took</Th>
              </Tr>
            </thead>
            <tbody>
              {chainRows(runs).map((row) =>
                row.kind === "loop" ? (
                  <Tr key={`loop-${row.run.id}`} className="bg-muted/40">
                    <Td colSpan={4} className="text-xs text-muted-foreground" data-testid="chain-loop">
                      ↩ loop · back to {row.run.flow_name} · round {row.run.round}
                      {row.run.rule ? ` · ${row.run.rule}` : ""}
                    </Td>
                  </Tr>
                ) : (
                  <Tr key={row.run.id} data-testid="chain-run">
                    <Td>
                      <StateBadge state={row.run.state} />{" "}
                      <Link
                        to="/runs/$runId"
                        params={{ runId: String(row.run.id) }}
                        className="hover:underline"
                      >
                        {row.run.flow_name} · {row.run.name}
                      </Link>
                      {row.run.round > 1 ? (
                        <span className="ml-1 text-xs text-muted-foreground">round {row.run.round}</span>
                      ) : null}
                    </Td>
                    <Td
                      className="max-w-[320px] truncate font-mono text-xs text-muted-foreground"
                      title={row.run.result ?? ""}
                    >
                      {row.run.result ?? ""}
                    </Td>
                    <Td className="font-mono text-xs">
                      {(row.run.attributes as { tokens?: number })?.tokens != null
                        ? formatTokens(Number((row.run.attributes as { tokens?: number }).tokens))
                        : ""}
                    </Td>
                    <Td className="text-xs text-muted-foreground">
                      {row.run.total_run_time != null
                        ? formatDuration(row.run.total_run_time)
                        : formatTime(row.run.created_at)}
                    </Td>
                  </Tr>
                ),
              )}
            </tbody>
          </Table>
        ) : (
          <div className="p-4 text-sm text-muted-foreground">
            {latestRunId == null
              ? "No runs yet."
              : "The last run was on its own. A chain appears here once a handoff has fired."}
          </div>
        )}
      </section>

      <Modal
        open={!!editing}
        onClose={() => setEditing(null)}
        title={
          editing?.rule
            ? `Handoff · ${editing.rule.name.replace(/^handoff: /, "")}`
            : `Handoff from ${flowName}`
        }
      >
        {editing ? (
          <HandoffEditor
            initial={editing.form}
            rules={all}
            targets={targets}
            excludeId={editing.rule?.id}
            busy={save.isPending}
            error={error}
            onCancel={() => setEditing(null)}
            onSave={(form, agentTarget, loop) =>
              save.mutate({ rule: editing.rule, body: ruleOf(form, agentTarget, loop) })
            }
          />
        ) : null}
      </Modal>
    </div>
  );
}
