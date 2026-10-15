import type { components } from "@/api/schema";

export type Rule = components["schemas"]["RuleRow"];
export type RuleAction = components["schemas"]["RuleAction"];
export type RuleBody = components["schemas"]["RuleBody"];
export type ChainRun = components["schemas"]["ChainRun"];
export type RunChain = components["schemas"]["RunChain"];

/** When the source ends, as the editor offers it. */
export type HandoffWhen = "completed" | "failed" | "any";

/** The editor's form: everything a handoff rule is built from. */
export type HandoffForm = {
  from: string;
  when: HandoffWhen;
  condition: string;
  to: string;
  /** For an agent target: the prompt template. */
  prompt: string;
  /** For a flow target: parameter templates by name. */
  parameters: { key: string; value: string }[];
  copyParameters: boolean;
  passResult: boolean;
  maxPerChain: number | null;
  delaySeconds: number | null;
  onLimitFlow: string;
};

export const TERMINAL_STATES = ["Completed", "Failed", "Cancelled", "Crashed"];

export function emptyHandoff(from: string): HandoffForm {
  return {
    from,
    when: "completed",
    condition: "",
    to: "",
    prompt: "",
    parameters: [],
    copyParameters: true,
    passResult: true,
    maxPerChain: null,
    delaySeconds: null,
    onLimitFlow: "",
  };
}

/** The run_flow actions of a rule. */
export function runFlowActions(rule: Pick<Rule, "do">): RuleAction[] {
  return (rule.do ?? []).filter((a) => a.kind === "run_flow");
}

/** A rule is a handoff of a flow when it fires on that flow and runs another. */
export function isHandoffFrom(rule: Rule, flow: string): boolean {
  const flows = rule.when?.flows ?? [];
  return flows.length === 1 && flows[0] === flow && runFlowActions(rule).length > 0;
}

export function isHandoffTo(rule: Rule, flow: string): boolean {
  return runFlowActions(rule).some((a) => a.flow === flow);
}

/** Outgoing and incoming handoffs of a flow, from every rule. */
export function handoffsOf(rules: Rule[], flow: string): { outgoing: Rule[]; incoming: Rule[] } {
  return {
    outgoing: rules.filter((r) => isHandoffFrom(r, flow)),
    incoming: rules.filter((r) => isHandoffTo(r, flow) && !isHandoffFrom(r, flow)),
  };
}

/** Whether running `to` leads back to `from` through the enabled rules' run_flow
 * actions (or `to` is `from`): the handoff would close a loop. */
export function reachesBack(rules: Rule[], from: string, to: string, excludeId?: number): boolean {
  if (to === from) return true;
  const edges = new Map<string, string[]>();
  for (const r of rules) {
    if (!r.enabled || r.id === excludeId) continue;
    for (const source of r.when?.flows ?? []) {
      for (const a of runFlowActions(r)) {
        if (a.flow) edges.set(source, [...(edges.get(source) ?? []), a.flow]);
      }
    }
  }
  const seen = new Set<string>();
  const stack = [to];
  while (stack.length) {
    const here = stack.pop() as string;
    if (here === from) return true;
    if (seen.has(here)) continue;
    seen.add(here);
    stack.push(...(edges.get(here) ?? []));
  }
  return false;
}

export function whenOf(rule: Pick<Rule, "when">): HandoffWhen {
  const events = rule.when?.events ?? [];
  if (events.length === 1 && events[0] === "run.completed") return "completed";
  if (events.every((e) => e === "run.failed" || e === "run.crashed") && events.length) return "failed";
  return "any";
}

export function describeWhen(when: HandoffWhen): string {
  return when === "completed"
    ? "on Completed"
    : when === "failed"
      ? "on Failed or Crashed"
      : "on any final state";
}

/** The form a stored handoff rule was built from. */
export function formOf(rule: Rule, from: string): HandoffForm {
  const action = runFlowActions(rule)[0];
  const params = Object.entries(action?.parameters ?? {});
  const prompt = params.find(([k]) => k === "prompt");
  return {
    from,
    when: whenOf(rule),
    condition: rule.condition ?? "",
    to: action?.flow ?? "",
    prompt: typeof prompt?.[1] === "string" ? prompt[1] : "",
    parameters: params.filter(([k]) => k !== "prompt").map(([key, value]) => ({ key, value: String(value) })),
    copyParameters: action?.copy_parameters ?? false,
    passResult: action?.pass_result !== false,
    maxPerChain: rule.max_per_chain ? rule.max_per_chain : null,
    delaySeconds: action?.delay ? action.delay : null,
    onLimitFlow: rule.on_limit?.kind === "run_flow" ? (rule.on_limit.flow ?? "") : "",
  };
}

/** The rule a form saves as. `agentTarget` says whether `to` is an agent, whose
 * prompt is a parameter; a flow target takes the parameter list. */
export function ruleOf(form: HandoffForm, agentTarget: boolean, loop: boolean): RuleBody {
  const when =
    form.when === "completed"
      ? { events: ["run.completed"], flows: [form.from] }
      : form.when === "failed"
        ? { events: ["run.failed", "run.crashed"], flows: [form.from] }
        : { events: ["run.*"], flows: [form.from], states: TERMINAL_STATES };
  const parameters: Record<string, string> = {};
  if (agentTarget && form.prompt.trim()) parameters.prompt = form.prompt;
  if (!agentTarget)
    for (const { key, value } of form.parameters) if (key.trim()) parameters[key.trim()] = value;
  const action: RuleAction = {
    kind: "run_flow",
    flow: form.to,
    parameters,
    copy_parameters: form.copyParameters,
    ...(agentTarget ? { pass_result: form.passResult } : {}),
    ...(loop && form.delaySeconds ? { delay: form.delaySeconds } : {}),
  } as RuleAction;
  return {
    name: `handoff: ${form.from} → ${form.to}`,
    enabled: true,
    when,
    do: [action],
    ...(form.condition.trim() ? { condition: form.condition.trim() } : {}),
    ...(loop ? { max_per_chain: form.maxPerChain ?? 0, allow_self: form.to === form.from } : {}),
    ...(loop && form.onLimitFlow
      ? { on_limit: { kind: "run_flow", flow: form.onLimitFlow, parameters: {} } }
      : {}),
  } as RuleBody;
}

/** Rows of a chain table: each run, and a loop row before a run that starts a
 * round over (a flow seen earlier in the chain). */
export function chainRows(
  runs: ChainRun[],
): ({ kind: "run"; run: ChainRun } | { kind: "loop"; run: ChainRun })[] {
  const rows: ({ kind: "run"; run: ChainRun } | { kind: "loop"; run: ChainRun })[] = [];
  for (const run of runs) {
    if (run.round > 1 && !/^(crash|retry):/.test(run.created_by ?? "")) rows.push({ kind: "loop", run });
    rows.push({ kind: "run", run });
  }
  return rows;
}

/** How many runs a rule added to a chain: its rounds used. */
export function roundsUsed(chain: ChainRun[] | undefined, ruleId: number): number {
  return (chain ?? []).filter((r) => r.rule_id === ruleId).length;
}
