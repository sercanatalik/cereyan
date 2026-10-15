import type { Run, TaskRun } from "@/api/client";
import { PausedBanner } from "@/components/paused-banner";
import { formatTokens } from "@/lib/agents";

type ModelSummary = {
  kind: "model";
  agent: string;
  model: string | null;
  usage: { input: number; output: number };
  text: string;
  tool_calls: { tool: string; args: unknown }[];
};
type ToolSummary = {
  kind: "tool";
  agent: string;
  tool: string;
  args: unknown;
  result: string;
  result_kind: string | null;
};
type Summary = ModelSummary | ToolSummary;

function summaryOf(t: TaskRun): Summary | null {
  const s = (t.state.details as { summary?: Summary } | undefined)?.summary;
  return s && (s.kind === "model" || s.kind === "tool") ? s : null;
}

/** Whether the run has agent units to show as a conversation. */
export function hasConversation(tasks: TaskRun[]): boolean {
  return tasks.some((t) => summaryOf(t) !== null);
}

function Mark({
  children,
  tone = "muted",
}: {
  children: React.ReactNode;
  tone?: "muted" | "green" | "blue" | "purple";
}) {
  const tones = {
    muted: "border text-muted-foreground",
    green: "bg-green-100 text-green-800 dark:bg-green-950 dark:text-green-300",
    blue: "bg-blue-100 text-blue-800 dark:bg-blue-950 dark:text-blue-300",
    purple: "bg-purple-100 text-purple-800 dark:bg-purple-950 dark:text-purple-300",
  };
  return <span className={`rounded px-1 text-[10px] ${tones[tone]}`}>{children}</span>;
}

function argsText(args: unknown): string {
  if (args && typeof args === "object" && !Array.isArray(args)) {
    return Object.entries(args as Record<string, unknown>)
      .map(([k, v]) => `${k}=${JSON.stringify(v)}`)
      .join(", ");
  }
  return typeof args === "string" ? args : JSON.stringify(args);
}

/** A run's agent units as turns: the prompt, each model request with its tool
 * calls and usage, each tool call with its result, and the question the run is
 * waiting on, if any, beside the call it is about. */
export function Conversation({
  run,
  tasks,
  onResumed,
}: {
  run: Run;
  tasks: TaskRun[];
  onResumed?: () => void;
}) {
  const prompt = (run.parameters as { prompt?: string } | undefined)?.prompt;
  const units = tasks.filter((t) => summaryOf(t)).sort((a, b) => a.id - b.id);
  const waiting = run.state.type === "Paused";
  let totalIn = 0;
  let totalOut = 0;
  let billedOut = 0;
  for (const t of units) {
    const s = summaryOf(t);
    if (s?.kind === "model") {
      totalIn += s.usage.input;
      totalOut += s.usage.output;
      if (t.state.name !== "Replayed") billedOut += s.usage.output;
    }
  }
  return (
    <div className="flex flex-col gap-3" data-testid="conversation">
      {prompt ? (
        <div className="flex gap-3">
          <span className="mt-0.5 rounded-md bg-muted px-1.5 py-0.5 font-mono text-[11px]">you</span>
          <div className="whitespace-pre-wrap text-sm">{prompt}</div>
        </div>
      ) : null}
      {units.map((t, i) => {
        const s = summaryOf(t) as Summary;
        const replayed = t.state.name === "Replayed";
        const recovered = t.state.name === "Recovered";
        const inDoubt = (t.state.details as { in_doubt?: string } | undefined)?.in_doubt === "deferred";
        const isLast = i === units.length - 1;
        return (
          <div key={t.id} className="flex gap-3" data-testid={`turn-${s.kind}`}>
            <span className="mt-0.5 rounded-md bg-foreground px-1.5 py-0.5 font-mono text-[11px] text-background">
              ai
            </span>
            <div className="flex min-w-0 flex-1 flex-col gap-1">
              <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                {s.kind === "model" ? (
                  <>
                    <span>model request</span>
                    {s.model ? <code className="font-mono">{s.model}</code> : null}
                    <span>
                      · {formatTokens(s.usage.input)} in / {formatTokens(s.usage.output)} out
                    </span>
                  </>
                ) : (
                  <>
                    <span>tool</span>
                    <code className="font-mono">{s.tool}</code>
                  </>
                )}
                {t.once ? <Mark>once</Mark> : null}
                {replayed ? <Mark tone="green">replayed · not billed</Mark> : null}
                {recovered ? <Mark tone="blue">recovered by probe</Mark> : null}
                {inDoubt ? <Mark tone="purple">in doubt · deferred to you</Mark> : null}
                {t.state.type === "Failed" ? <Mark>failed</Mark> : null}
              </div>
              {s.kind === "model" ? (
                <>
                  {s.text ? <div className="whitespace-pre-wrap text-sm">{s.text}</div> : null}
                  {s.tool_calls.map((c) => (
                    <div
                      key={`${c.tool}:${JSON.stringify(c.args)}`}
                      className="rounded-md border bg-muted/40 px-2.5 py-1.5 font-mono text-xs"
                    >
                      → {c.tool}({argsText(c.args)})
                    </div>
                  ))}
                </>
              ) : (
                <div className="rounded-md border bg-muted/40 px-2.5 py-1.5 font-mono text-xs">
                  <div>
                    {s.tool}({argsText(s.args)})
                  </div>
                  <div className="text-muted-foreground">← {s.result}</div>
                </div>
              )}
              {waiting && isLast ? <PausedBanner run={run} onResumed={onResumed} /> : null}
            </div>
          </div>
        );
      })}
      {waiting && units.length === 0 ? <PausedBanner run={run} onResumed={onResumed} /> : null}
      <div className="border-t pt-2 text-xs text-muted-foreground" data-testid="conversation-usage">
        {units.filter((t) => summaryOf(t)?.kind === "model").length} model requests · {formatTokens(totalIn)}{" "}
        in / {formatTokens(totalOut)} out
        {totalOut !== billedOut ? ` · ${formatTokens(totalOut - billedOut)} out replayed, not billed` : ""}
      </div>
    </div>
  );
}
