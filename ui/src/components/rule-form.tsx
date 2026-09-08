import { useState } from "react";
import type { components } from "@/api/schema";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input, Select, Textarea } from "@/components/ui/input";

export type RuleSpec = components["schemas"]["RuleSpec"];
export type RuleAction = components["schemas"]["RuleAction"];

export const ACTION_KINDS = [
  "run_flow",
  "cancel_run",
  "set_state",
  "pause_schedule",
  "resume_schedule",
  "webhook",
  "email",
];

export function summarizeWhen(when: RuleSpec["when"]): string {
  const parts = [when.events?.length ? when.events.join(", ") : "any event"];
  if (when.flows?.length) parts.push(`flows ${when.flows.join(", ")}`);
  if (when.tags?.length) parts.push(`tags ${when.tags.join(", ")}`);
  if (when.states?.length) parts.push(`states ${when.states.join(", ")}`);
  if (when.project) parts.push(`project ${when.project}`);
  return parts.join(" · ");
}

export function summarizeDo(actions: RuleAction[]): string {
  return actions
    .map((a) => {
      switch (a.kind) {
        case "run_flow":
          return `run ${a.flow}`;
        case "webhook":
          return `webhook ${a.url}`;
        case "email":
          return `email ${(a.to ?? []).join(", ")}`;
        case "set_state":
          return `set ${a.state_type}`;
        case "call":
          return `call ${a.callable}`;
        default:
          return a.kind;
      }
    })
    .join(" → ");
}

/** One line for a proactive rule's expectation, or "" for reactive rules. */
export function summarizeUnless(rule: Pick<RuleSpec, "unless" | "within" | "at">): string {
  if (!rule.unless) return "";
  const expected = (rule.unless.events ?? []).join(", ") || "expected event";
  if (rule.at) {
    return `unless ${expected} by ${rule.at.cron}${rule.at.tz ? ` (${rule.at.tz})` : ""}${
      rule.within ? ` in the last ${rule.within}s` : ""
    }`;
  }
  return `unless ${expected} within ${rule.within ?? 0}s`;
}

export function emptyRule(): { name: string; enabled: boolean } & RuleSpec {
  return {
    name: "",
    enabled: true,
    when: { events: ["run.failed"], flows: [], tags: [], states: [], project: null },
    do: [
      { kind: "webhook", url: "", body: "", parameters: {}, headers: {}, to: [] } as unknown as RuleAction,
    ],
    once: "per_run",
    cooldown_seconds: 0,
    max_per_minute: 60,
    allow_self: false,
    unless: null,
    within: null,
    at: null,
  };
}

function list(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

export function RuleForm({
  initial,
  onSave,
  onCancel,
  busy,
  error,
}: {
  initial?: { name: string; enabled: boolean } & RuleSpec;
  onSave: (rule: { name: string; enabled: boolean } & RuleSpec) => void;
  onCancel?: () => void;
  busy?: boolean;
  error?: string | null;
}) {
  const [rule, setRule] = useState(initial ?? emptyRule());
  const setWhen = (patch: Partial<RuleSpec["when"]>) =>
    setRule({ ...rule, when: { ...rule.when, ...patch } });
  const setAction = (i: number, patch: Partial<RuleAction>) => {
    const actions = rule.do.slice();
    actions[i] = { ...actions[i], ...patch };
    setRule({ ...rule, do: actions });
  };
  const move = (i: number, dir: -1 | 1) => {
    const actions = rule.do.slice();
    const j = i + dir;
    if (j < 0 || j >= actions.length) return;
    [actions[i], actions[j]] = [actions[j], actions[i]];
    setRule({ ...rule, do: actions });
  };
  return (
    <div className="space-y-4" data-testid="rule-form">
      <label className="block text-xs text-muted-foreground" htmlFor="rule-name">
        Name
        <Input
          id="rule-name"
          className="mt-1"
          value={rule.name}
          onChange={(e) => setRule({ ...rule, name: e.target.value })}
        />
      </label>
      <fieldset className="space-y-2 rounded-md border p-3">
        <legend className="px-1 text-xs font-medium">When</legend>
        <label className="block text-xs text-muted-foreground" htmlFor="rule-events">
          Events (names or prefixes like run.*, comma separated)
          <Input
            id="rule-events"
            className="mt-1"
            value={(rule.when.events ?? []).join(", ")}
            onChange={(e) => setWhen({ events: list(e.target.value) })}
          />
        </label>
        <div className="grid grid-cols-2 gap-2">
          <label className="block text-xs text-muted-foreground" htmlFor="rule-flows">
            Flows
            <Input
              id="rule-flows"
              className="mt-1"
              value={(rule.when.flows ?? []).join(", ")}
              onChange={(e) => setWhen({ flows: list(e.target.value) })}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="rule-tags">
            Tags
            <Input
              id="rule-tags"
              className="mt-1"
              value={(rule.when.tags ?? []).join(", ")}
              onChange={(e) => setWhen({ tags: list(e.target.value) })}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="rule-states">
            States
            <Input
              id="rule-states"
              className="mt-1"
              value={(rule.when.states ?? []).join(", ")}
              onChange={(e) => setWhen({ states: list(e.target.value) })}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="rule-project">
            Project
            <Input
              id="rule-project"
              className="mt-1"
              value={rule.when.project ?? ""}
              onChange={(e) => setWhen({ project: e.target.value || null })}
            />
          </label>
        </div>
      </fieldset>
      <fieldset className="space-y-2 rounded-md border p-3">
        <legend className="px-1 text-xs font-medium">Do</legend>
        {rule.do
          .map((a, i) => ({ a, i, key: `${i}:${a.kind}` }))
          .map(({ a, i, key }) => (
            <div key={key} className="space-y-2 rounded border p-2" data-testid="rule-action">
              <div className="flex items-center gap-2">
                <Select
                  value={a.kind}
                  onChange={(e) => setAction(i, { kind: e.target.value })}
                  aria-label="Action kind"
                >
                  {ACTION_KINDS.map((k) => (
                    <option key={k} value={k}>
                      {k}
                    </option>
                  ))}
                </Select>
                <span className="flex-1" />
                <Button size="sm" variant="ghost" onClick={() => move(i, -1)} aria-label="Move up">
                  ↑
                </Button>
                <Button size="sm" variant="ghost" onClick={() => move(i, 1)} aria-label="Move down">
                  ↓
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => setRule({ ...rule, do: rule.do.filter((_, j) => j !== i) })}
                  aria-label="Remove action"
                >
                  ×
                </Button>
              </div>
              {a.kind === "run_flow" ? (
                <>
                  <Input
                    placeholder="flow name"
                    value={a.flow ?? ""}
                    onChange={(e) => setAction(i, { flow: e.target.value })}
                    aria-label="Flow"
                  />
                  <Textarea
                    rows={2}
                    placeholder='parameters JSON with templates, e.g. {"day": "{{ run.parameters.day }}"}'
                    value={JSON.stringify(a.parameters ?? {})}
                    onChange={(e) => {
                      try {
                        setAction(i, { parameters: JSON.parse(e.target.value) });
                      } catch {}
                    }}
                    aria-label="Parameters"
                  />
                </>
              ) : null}
              {a.kind === "set_state" ? (
                <div className="flex gap-2">
                  <Input
                    placeholder="state type"
                    value={a.state_type ?? ""}
                    onChange={(e) => setAction(i, { state_type: e.target.value })}
                    aria-label="State type"
                  />
                  <Input
                    placeholder="message"
                    value={a.message ?? ""}
                    onChange={(e) => setAction(i, { message: e.target.value })}
                    aria-label="Message"
                  />
                </div>
              ) : null}
              {a.kind === "pause_schedule" || a.kind === "resume_schedule" ? (
                <Input
                  placeholder="flow (defaults to the event's flow)"
                  value={a.flow ?? ""}
                  onChange={(e) => setAction(i, { flow: e.target.value })}
                  aria-label="Flow"
                />
              ) : null}
              {a.kind === "webhook" ? (
                <>
                  <Input
                    placeholder="https://..."
                    value={a.url ?? ""}
                    onChange={(e) => setAction(i, { url: e.target.value })}
                    aria-label="URL"
                  />
                  <Textarea
                    rows={2}
                    placeholder="body template (default: the event as JSON)"
                    value={a.body ?? ""}
                    onChange={(e) => setAction(i, { body: e.target.value })}
                    aria-label="Body"
                  />
                </>
              ) : null}
              {a.kind === "email" ? (
                <>
                  <Input
                    placeholder="to, comma separated"
                    value={(a.to ?? []).join(", ")}
                    onChange={(e) => setAction(i, { to: list(e.target.value) })}
                    aria-label="To"
                  />
                  <Input
                    placeholder="subject template"
                    value={a.subject ?? ""}
                    onChange={(e) => setAction(i, { subject: e.target.value })}
                    aria-label="Subject"
                  />
                  <Textarea
                    rows={3}
                    placeholder="body template (default: failure summary)"
                    value={a.body ?? ""}
                    onChange={(e) => setAction(i, { body: e.target.value })}
                    aria-label="Email body"
                  />
                </>
              ) : null}
            </div>
          ))}
        <Button
          size="sm"
          variant="outline"
          onClick={() =>
            setRule({
              ...rule,
              do: [
                ...rule.do,
                { kind: "webhook", url: "", parameters: {}, headers: {}, to: [] } as unknown as RuleAction,
              ],
            })
          }
        >
          Add action
        </Button>
      </fieldset>
      <fieldset className="space-y-2 rounded-md border p-3" data-testid="rule-unless">
        <legend className="px-1 text-xs font-medium">Unless (proactive)</legend>
        <label className="block text-xs text-muted-foreground" htmlFor="rule-unless-events">
          Expected events (leave empty for a reactive rule)
          <Input
            id="rule-unless-events"
            className="mt-1"
            value={(rule.unless?.events ?? []).join(", ")}
            onChange={(e) => {
              const events = list(e.target.value);
              setRule({
                ...rule,
                unless: events.length
                  ? { events, flows: rule.when.flows, tags: [], states: [], project: rule.when.project }
                  : null,
              });
            }}
          />
        </label>
        <div className="grid grid-cols-3 gap-2">
          <label className="block text-xs text-muted-foreground" htmlFor="rule-within">
            Within (seconds)
            <Input
              id="rule-within"
              type="number"
              className="mt-1"
              value={rule.within ?? ""}
              onChange={(e) => setRule({ ...rule, within: e.target.value ? Number(e.target.value) : null })}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="rule-at">
            At (cron; clears the when events)
            <Input
              id="rule-at"
              className="mt-1"
              placeholder="0 9 * * *"
              value={rule.at?.cron ?? ""}
              onChange={(e) =>
                setRule({
                  ...rule,
                  at: e.target.value ? { cron: e.target.value, tz: rule.at?.tz ?? null } : null,
                  when: e.target.value ? { ...rule.when, events: [] } : rule.when,
                })
              }
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="rule-tz">
            Timezone
            <Input
              id="rule-tz"
              className="mt-1"
              placeholder="UTC"
              value={rule.at?.tz ?? ""}
              onChange={(e) =>
                setRule({ ...rule, at: { cron: rule.at?.cron ?? "", tz: e.target.value || null } })
              }
            />
          </label>
        </div>
      </fieldset>
      <fieldset className="grid grid-cols-4 gap-2 rounded-md border p-3">
        <legend className="px-1 text-xs font-medium">Guards</legend>
        <label className="text-xs text-muted-foreground" htmlFor="rule-once">
          Once
          <Select
            id="rule-once"
            className="mt-1 w-full"
            value={rule.once}
            onChange={(e) => setRule({ ...rule, once: e.target.value })}
          >
            <option value="per_run">per run</option>
            <option value="never">never</option>
          </Select>
        </label>
        <label className="text-xs text-muted-foreground" htmlFor="rule-cooldown">
          Cooldown (s)
          <Input
            id="rule-cooldown"
            className="mt-1"
            value={rule.cooldown_seconds}
            onChange={(e) => setRule({ ...rule, cooldown_seconds: Number(e.target.value) || 0 })}
          />
        </label>
        <label className="text-xs text-muted-foreground" htmlFor="rule-rate">
          Max per minute
          <Input
            id="rule-rate"
            className="mt-1"
            value={rule.max_per_minute}
            onChange={(e) => setRule({ ...rule, max_per_minute: Number(e.target.value) || 0 })}
          />
        </label>
        <span className="flex items-end gap-2 text-xs text-muted-foreground">
          <Checkbox
            checked={rule.allow_self}
            onCheckedChange={(c) => setRule({ ...rule, allow_self: c === true })}
            aria-label="allow self-trigger"
          />
          allow self-trigger
        </span>
      </fieldset>
      {error ? <div className="text-xs text-red-600">{error}</div> : null}
      <div className="flex justify-end gap-2">
        {onCancel ? (
          <Button variant="outline" onClick={onCancel}>
            Cancel
          </Button>
        ) : null}
        <Button onClick={() => onSave(rule)} disabled={busy || !rule.name}>
          Save rule
        </Button>
      </div>
    </div>
  );
}
