import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input, Select, Textarea } from "@/components/ui/input";
import { type HandoffForm, type HandoffWhen, type Rule, reachesBack } from "@/lib/handoffs";

export type Target = { name: string; kind: "agent" | "flow"; description?: string };

/** The editor over one handoff: when the source ends, only when, which agent
 * or flow runs next with what, and, when the target points back up the chain,
 * the bound that keeps the loop finite. */
export function HandoffEditor({
  initial,
  rules,
  targets,
  excludeId,
  onSave,
  onCancel,
  busy,
  error,
}: {
  initial: HandoffForm;
  rules: Rule[];
  targets: Target[];
  excludeId?: number;
  onSave: (form: HandoffForm, agentTarget: boolean, loop: boolean) => void;
  onCancel: () => void;
  busy?: boolean;
  error?: string | null;
}) {
  const [form, setForm] = useState<HandoffForm>(initial);
  const set = (patch: Partial<HandoffForm>) => setForm({ ...form, ...patch });
  const target = targets.find((t) => t.name === form.to);
  const agentTarget = target?.kind === "agent";
  const loop = !!form.to && reachesBack(rules, form.from, form.to, excludeId);
  const missingBound = loop && !(form.maxPerChain && form.maxPerChain > 0);
  const canSave = !!form.to && !missingBound && !busy;
  return (
    <div className="flex flex-col gap-4" data-testid="handoff-editor">
      <label className="block text-xs text-muted-foreground" htmlFor="handoff-when">
        When {form.from} ends
        <Select
          id="handoff-when"
          className="mt-1"
          value={form.when}
          onChange={(e) => set({ when: e.target.value as HandoffWhen })}
        >
          <option value="completed">Completed</option>
          <option value="failed">Failed or crashed</option>
          <option value="any">Any final state</option>
        </Select>
      </label>
      <label className="block text-xs text-muted-foreground" htmlFor="handoff-condition">
        Only when (optional, an expression over the result)
        <Input
          id="handoff-condition"
          className="mt-1 font-mono"
          placeholder="not result.approved"
          value={form.condition}
          onChange={(e) => set({ condition: e.target.value })}
        />
      </label>
      <label className="block text-xs text-muted-foreground" htmlFor="handoff-to">
        Run
        <Select
          id="handoff-to"
          className="mt-1"
          value={form.to}
          onChange={(e) => set({ to: e.target.value })}
          data-testid="handoff-to"
        >
          <option value="">Choose an agent or a flow</option>
          {targets.map((t) => (
            <option key={t.name} value={t.name}>
              {t.name} · {t.kind}
              {reachesBack(rules, form.from, t.name, excludeId) ? " (back up the chain)" : ""}
            </option>
          ))}
        </Select>
      </label>
      {agentTarget ? (
        <label className="block text-xs text-muted-foreground" htmlFor="handoff-prompt">
          With prompt
          <Textarea
            id="handoff-prompt"
            aria-label="With prompt"
            className="mt-1 font-mono text-xs"
            rows={3}
            placeholder="Check the blame in today's digest. The reviewer said: {{ result.notes }}"
            value={form.prompt}
            onChange={(e) => set({ prompt: e.target.value })}
          />
          <span className="mt-1 block">
            Templates see <code>result</code>, <code>result.&lt;field&gt;</code> for JSON output,{" "}
            <code>run</code>, <code>parameters</code>, <code>event</code> and <code>loop.count</code>. The
            whole result also reaches the agent as data, so the prompt says what to do with it.
          </span>
        </label>
      ) : form.to ? (
        <fieldset className="flex flex-col gap-2 text-xs text-muted-foreground">
          <legend>With parameters (templated)</legend>
          {form.parameters.map((p, i) => (
            <div key={`${i}-${p.key}`} className="flex gap-2">
              <Input
                aria-label={`Parameter ${i + 1} name`}
                className="w-1/3 font-mono"
                value={p.key}
                onChange={(e) => {
                  const parameters = form.parameters.slice();
                  parameters[i] = { ...p, key: e.target.value };
                  set({ parameters });
                }}
              />
              <Input
                aria-label={`Parameter ${i + 1} value`}
                className="flex-1 font-mono"
                value={p.value}
                onChange={(e) => {
                  const parameters = form.parameters.slice();
                  parameters[i] = { ...p, value: e.target.value };
                  set({ parameters });
                }}
              />
            </div>
          ))}
          <Button
            variant="outline"
            size="sm"
            className="self-start"
            onClick={() => set({ parameters: [...form.parameters, { key: "", value: "{{ result }}" }] })}
          >
            Add parameter
          </Button>
        </fieldset>
      ) : null}
      {loop ? (
        <fieldset className="flex flex-col gap-2 rounded-md border p-3 text-xs" data-testid="loop-limits">
          <legend className="px-1 font-medium">Loop limits</legend>
          <span className="text-muted-foreground">
            This handoff points back up the chain, so it needs a bound. Rounds are counted along the chain's
            links, so a crash rerun does not count as a new round.
          </span>
          <div className="grid grid-cols-2 gap-2">
            <label className="block text-muted-foreground" htmlFor="handoff-max">
              At most (rounds per chain)
              <Input
                id="handoff-max"
                className="mt-1"
                type="number"
                min={1}
                value={form.maxPerChain ?? ""}
                onChange={(e) => set({ maxPerChain: e.target.value ? Number(e.target.value) : null })}
              />
            </label>
            <label className="block text-muted-foreground" htmlFor="handoff-delay">
              Wait between rounds (seconds)
              <Input
                id="handoff-delay"
                className="mt-1"
                type="number"
                min={0}
                value={form.delaySeconds ?? ""}
                onChange={(e) => set({ delaySeconds: e.target.value ? Number(e.target.value) : null })}
              />
            </label>
          </div>
          <label className="block text-muted-foreground" htmlFor="handoff-on-limit">
            When the limit is reached
            <Select
              id="handoff-on-limit"
              className="mt-1"
              value={form.onLimitFlow}
              onChange={(e) => set({ onLimitFlow: e.target.value })}
            >
              <option value="">Stop the chain and record it</option>
              {targets.map((t) => (
                <option key={t.name} value={t.name}>
                  Run {t.name}
                </option>
              ))}
            </Select>
          </label>
        </fieldset>
      ) : null}
      <fieldset className="flex flex-col gap-2 text-xs">
        <legend className="text-muted-foreground">Pass along</legend>
        <label className="flex items-center gap-2" htmlFor="handoff-copy">
          <Checkbox
            id="handoff-copy"
            checked={form.copyParameters}
            onCheckedChange={(v) => set({ copyParameters: v === true })}
          />
          Parameters by name (such as a shared <code>day</code>)
        </label>
        {agentTarget ? (
          <label className="flex items-center gap-2" htmlFor="handoff-pass">
            <Checkbox
              id="handoff-pass"
              checked={form.passResult}
              onCheckedChange={(v) => set({ passResult: v === true })}
            />
            The result, as data marked from {form.from}, not as instructions
          </label>
        ) : null}
      </fieldset>
      {error ? (
        <div className="rounded-md border border-destructive/50 bg-destructive/10 p-2 text-xs" role="alert">
          {error}
        </div>
      ) : null}
      <div className="flex justify-end gap-2">
        <Button variant="outline" size="sm" onClick={onCancel}>
          Cancel
        </Button>
        <Button
          size="sm"
          disabled={!canSave}
          onClick={() => onSave(form, agentTarget, loop)}
          data-testid="save-handoff"
          title={missingBound ? "Set how many rounds the loop may take" : undefined}
        >
          Save handoff
        </Button>
      </div>
    </div>
  );
}
