import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { JsonView } from "@/components/json-view";
import { Page } from "@/components/shell";
import { StateBadge } from "@/components/state-badge";
import { Card, CardContent, CardHead } from "@/components/ui/card";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { cn, formatDuration, formatTime } from "@/lib/utils";

type Comparison = components["schemas"]["RunComparison"];
type Search = { ids?: string };

export const Route = createFileRoute("/runs/compare")({
  validateSearch: (s: Record<string, unknown>): Search => ({
    ids: typeof s.ids === "string" ? s.ids : undefined,
  }),
  component: ComparePage,
});

/** A signed duration delta: "+2.3s" or "−400ms". */
export function formatDelta(micros: number | null | undefined): string {
  if (micros == null) return "";
  if (micros === 0) return "±0";
  return (micros > 0 ? "+" : "−") + formatDuration(Math.abs(micros));
}

function RunHead({ run, side }: { run: Comparison["left"]; side: "Baseline" | "Compared" }) {
  return (
    <div className="flex min-w-0 flex-col gap-1" data-testid={`compare-${side.toLowerCase()}`}>
      <span className="text-xs uppercase tracking-wide text-muted-foreground">{side}</span>
      <div className="flex flex-wrap items-center gap-2">
        <Link to="/runs/$runId" params={{ runId: String(run.id) }} className="font-semibold hover:underline">
          {run.name}
        </Link>
        <StateBadge state={run.state} />
        <span className="text-muted-foreground">
          {run.project}/{run.flow_name}
        </span>
      </div>
      <span className="text-xs text-muted-foreground">
        {formatTime(run.start_time ?? run.created_at)} · {formatDuration(run.total_run_time)}
      </span>
    </div>
  );
}

function ComparePage() {
  const { ids } = Route.useSearch();
  const pair = (ids ?? "")
    .split(",")
    .map((s) => Number(s.trim()))
    .filter((n) => Number.isInteger(n) && n > 0);
  const query = useQuery({
    queryKey: ["compare", ids],
    queryFn: async () =>
      unwrap(await api.GET("/api/runs/compare", { params: { query: { ids: ids ?? "" } } })),
    enabled: pair.length === 2,
  });
  const c = query.data;
  const crumbs = [{ label: "Runs", to: "/runs" }, { label: "Compare" }];
  if (pair.length !== 2) {
    return (
      <Page crumbs={crumbs} title="Compare runs" width="narrow">
        <p className="text-sm text-muted-foreground">
          Pick two runs on the Runs page and click Compare, or open{" "}
          <code className="font-mono">/runs/compare?ids=a,b</code>.
        </p>
      </Page>
    );
  }
  if (query.isError) {
    return (
      <Page crumbs={crumbs} title="Compare runs" width="narrow">
        <p className="text-sm text-destructive">{String(query.error)}</p>
      </Page>
    );
  }
  if (!c)
    return (
      <Page crumbs={crumbs} title="Compare runs" width="narrow">
        <p className="text-sm text-muted-foreground">Comparing…</p>
      </Page>
    );
  const s = c.summary;
  const changedParams = c.parameters.filter((p) => p.changed);
  const changedAttrs = c.attributes.filter((p) => p.changed);
  return (
    <Page
      crumbs={crumbs}
      title="Compare runs"
      subtitle={
        <span data-testid="compare-summary">
          {s.parameters_changed} parameter{s.parameters_changed === 1 ? "" : "s"} changed ·{" "}
          {s.tasks_state_changed} task state{s.tasks_state_changed === 1 ? "" : "s"} changed ·{" "}
          {s.tasks_duration_changed} slower or faster · {s.new_errors} new error
          {s.new_errors === 1 ? "" : "s"} · {s.artifacts_changed} artifact difference
          {s.artifacts_changed === 1 ? "" : "s"}
          {c.same_flow ? "" : " · different flows"}
        </span>
      }
      width="narrow"
    >
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
        <RunHead run={c.left} side="Baseline" />
        <RunHead run={c.right} side="Compared" />
      </div>
      <div className="text-sm">
        Duration {formatDuration(c.duration.left)} → {formatDuration(c.duration.right)}{" "}
        <span
          className={cn(
            "tabular-nums",
            (c.duration.delta ?? 0) > 0 ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {formatDelta(c.duration.delta)}
        </span>
      </div>
      <Card className="gap-0 py-0">
        <CardHead title="Parameters" />
        <CardContent className="p-0">
          {changedParams.length === 0 && changedAttrs.length === 0 ? (
            <p className="p-4 text-sm text-muted-foreground">Same parameters and attributes.</p>
          ) : (
            <Table>
              <thead>
                <Tr>
                  <Th>Key</Th>
                  <Th>Baseline</Th>
                  <Th>Compared</Th>
                </Tr>
              </thead>
              <tbody>
                {changedParams.map((p) => (
                  <Tr key={`p-${p.key}`} data-testid="param-diff">
                    <Td className="font-mono text-xs">{p.key}</Td>
                    <Td className="font-mono text-xs">{JSON.stringify(p.left)}</Td>
                    <Td className="font-mono text-xs">{JSON.stringify(p.right)}</Td>
                  </Tr>
                ))}
                {changedAttrs.map((p) => (
                  <Tr key={`a-${p.key}`} data-testid="attribute-diff">
                    <Td className="font-mono text-xs">
                      {p.key} <span className="text-muted-foreground">(attribute)</span>
                    </Td>
                    <Td className="font-mono text-xs">{JSON.stringify(p.left)}</Td>
                    <Td className="font-mono text-xs">{JSON.stringify(p.right)}</Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
          )}
        </CardContent>
      </Card>
      <Card className="gap-0 py-0">
        <CardHead title="Tasks" />
        <CardContent className="p-0">
          {c.tasks.length === 0 ? (
            <p className="p-4 text-sm text-muted-foreground">Neither run recorded task runs.</p>
          ) : (
            <Table>
              <thead>
                <Tr>
                  <Th>Task</Th>
                  <Th>Baseline</Th>
                  <Th>Compared</Th>
                  <Th className="text-right">Δ</Th>
                </Tr>
              </thead>
              <tbody>
                {c.tasks.map((t) => {
                  const first = t.key === c.first_divergence;
                  return (
                    <Tr
                      key={t.key}
                      className={cn(first && "bg-amber-50 dark:bg-amber-950/30")}
                      data-testid={first ? "first-divergence" : undefined}
                    >
                      <Td className="font-mono text-xs">
                        {t.key}
                        {first ? (
                          <span className="ml-2 text-amber-700 dark:text-amber-300">first divergence</span>
                        ) : null}
                      </Td>
                      <Td>
                        {t.left ? (
                          <span className="flex items-center gap-2">
                            <StateBadge state={t.left.state} /> {formatDuration(t.left.duration)}
                          </span>
                        ) : (
                          <span className="text-muted-foreground">not run</span>
                        )}
                      </Td>
                      <Td>
                        {t.right ? (
                          <span className="flex items-center gap-2">
                            <StateBadge state={t.right.state} /> {formatDuration(t.right.duration)}
                          </span>
                        ) : (
                          <span className="text-muted-foreground">not run</span>
                        )}
                      </Td>
                      <Td className={cn("text-right tabular-nums", (t.delta ?? 0) > 0 && "text-destructive")}>
                        {formatDelta(t.delta)}
                      </Td>
                    </Tr>
                  );
                })}
              </tbody>
            </Table>
          )}
        </CardContent>
      </Card>
      <Card className="gap-0 py-0">
        <CardHead title="New errors" />
        <CardContent className="p-4">
          {c.new_errors.length === 0 ? (
            <p className="text-sm text-muted-foreground">No error the baseline did not have.</p>
          ) : (
            <ul className="space-y-1 font-mono text-xs" data-testid="new-errors">
              {c.new_errors.map((m) => (
                <li key={m} className="whitespace-pre-wrap">
                  {m}
                </li>
              ))}
            </ul>
          )}
        </CardContent>
      </Card>
      <Card className="gap-0 py-0">
        <CardHead title="Artifacts" />
        <CardContent className="p-0">
          {c.artifacts.length === 0 ? (
            <p className="p-4 text-sm text-muted-foreground">Neither run published artifacts.</p>
          ) : (
            <Table>
              <thead>
                <Tr>
                  <Th>Artifact</Th>
                  <Th>Baseline</Th>
                  <Th>Compared</Th>
                  <Th>Changed</Th>
                </Tr>
              </thead>
              <tbody>
                {c.artifacts.map((a, i) => (
                  <Tr key={`${a.kind}-${a.key ?? i}`}>
                    <Td className="font-mono text-xs">
                      {a.key ?? <span className="text-muted-foreground">{a.kind}</span>}
                    </Td>
                    <Td className="text-xs">{a.left ? `#${a.left}` : "—"}</Td>
                    <Td className="text-xs">{a.right ? `#${a.right}` : "—"}</Td>
                    <Td className="text-xs">{a.changed ? "yes" : "no"}</Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
          )}
        </CardContent>
      </Card>
      <details className="text-xs text-muted-foreground">
        <summary className="cursor-pointer">Raw comparison</summary>
        <JsonView value={c} />
      </details>
    </Page>
  );
}
