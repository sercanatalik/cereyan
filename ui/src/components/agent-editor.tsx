import { useMutation, useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { ApiError, api, unwrap } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Textarea } from "@/components/ui/textarea";
import { type AgentDefinition, EMPTY_DEFINITION, type ToolSpec, useCatalogue } from "@/lib/agents";

function Section({ title, hint, children }: { title: string; hint?: string; children: React.ReactNode }) {
  return (
    <section className="flex flex-col gap-3 rounded-lg border bg-card p-4" aria-label={title}>
      <div>
        <h2 className="text-sm font-semibold">{title}</h2>
        {hint ? <div className="text-xs text-muted-foreground">{hint}</div> : null}
      </div>
      {children}
    </section>
  );
}

function Field({
  label,
  htmlFor,
  hint,
  children,
}: {
  label: string;
  htmlFor: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={htmlFor} className="text-xs text-muted-foreground">
        {label}
      </label>
      {children}
      {hint ? <div className="text-xs text-muted-foreground">{hint}</div> : null}
    </div>
  );
}

/** A tool row of the picker: a checkbox, a name, and what it needs. */
function ToolRow({
  id,
  checked,
  onChange,
  name,
  description,
  needs,
  disabled,
  tags,
  children,
}: {
  id: string;
  checked: boolean;
  onChange: (on: boolean) => void;
  name: string;
  description?: string;
  needs?: string[];
  disabled?: boolean;
  tags?: string[];
  children?: React.ReactNode;
}) {
  return (
    <div className="flex items-start gap-3 rounded-md border p-2.5" data-testid={`tool-${id}`}>
      <Checkbox
        id={id}
        checked={checked}
        onCheckedChange={(v) => onChange(v === true)}
        disabled={disabled}
        className="mt-0.5"
      />
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <label htmlFor={id} className="text-sm font-medium">
          {name}
          {tags?.map((t) => (
            <span key={t} className="ml-1.5 rounded border px-1 text-[10px] text-muted-foreground">
              {t}
            </span>
          ))}
        </label>
        {description ? <div className="text-xs text-muted-foreground">{description}</div> : null}
        {needs?.length ? (
          <div className="text-xs text-amber-700 dark:text-amber-400">needs {needs.join(", ")}</div>
        ) : null}
        {children}
      </div>
    </div>
  );
}

/** The editor for an agent definition: new, or an existing one by id. */
export function AgentEditor({ id, initial }: { id?: number; initial?: AgentDefinition }) {
  const navigate = useNavigate();
  const [d, setD] = useState<AgentDefinition>(initial ?? EMPTY_DEFINITION);
  const [schemaText, setSchemaText] = useState(() =>
    JSON.stringify(initial?.output.schema ?? { type: "object", properties: {} }, null, 2),
  );
  const [mcpDraft, setMcpDraft] = useState({ name: "", url: "", command: "", args: "" });
  const [error, setError] = useState<string | null>(null);
  const catalogue = useCatalogue();
  const variables = useQuery({
    queryKey: ["variables"],
    queryFn: async () => unwrap(await api.GET("/api/variables")),
  });
  const secrets = (variables.data ?? []).filter((v) => v.secret).map((v) => v.name);
  const cat = catalogue.data;
  const set = (patch: Partial<AgentDefinition>) => setD((prev) => ({ ...prev, ...patch }));
  const hasTool = (kind: string, name: string) => d.tools.some((t) => t.kind === kind && t.name === name);
  const toggleTool = (tool: ToolSpec, on: boolean) =>
    set({
      tools: on
        ? [...d.tools.filter((t) => !(t.kind === tool.kind && t.name === tool.name)), tool]
        : d.tools.filter((t) => !(t.kind === tool.kind && t.name === tool.name)),
    });
  const selfTool = d.tools.find((t) => t.kind === "self");
  const selfNames = ((selfTool?.options as { tools?: string[] } | undefined)?.tools ?? []) as string[];
  const toggleSelfName = (name: string, on: boolean) => {
    const names = on ? [...selfNames.filter((n) => n !== name), name] : selfNames.filter((n) => n !== name);
    toggleTool({ kind: "self", name: "cereyan", options: { tools: names } }, true);
  };
  const toolNames = d.tools.flatMap((t) => (t.kind === "self" ? selfNames : [t.name]));

  const save = useMutation({
    mutationFn: async () => {
      let schema: unknown = null;
      if (d.output.kind === "json") {
        try {
          schema = JSON.parse(schemaText);
        } catch (e) {
          throw new Error(`output.schema: ${e instanceof Error ? e.message : String(e)}`);
        }
      }
      const body = { ...d, group: d.group || null, output: { kind: d.output.kind, schema } } as any;
      const r =
        id != null
          ? await api.PATCH("/api/agents/{id}", { params: { path: { id } }, body })
          : await api.POST("/api/agents", { body });
      return unwrap(r as any);
    },
    onSuccess: (agent: any) => {
      setError(null);
      navigate({ to: "/agents/$agentId", params: { agentId: String(agent.id) } });
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : e instanceof Error ? e.message : String(e)),
  });

  const allowlist = cat?.allowlist ?? { mcp_commands: [], mcp_urls: [], cost_per_day: 0 };
  const addMcp = () => {
    if (!mcpDraft.name) return;
    const options: Record<string, unknown> = mcpDraft.url
      ? { url: mcpDraft.url }
      : { command: mcpDraft.command, args: mcpDraft.args.split(/\s+/).filter(Boolean) };
    toggleTool({ kind: "mcp", name: mcpDraft.name, options }, true);
    setMcpDraft({ name: "", url: "", command: "", args: "" });
  };

  return (
    <form
      className="grid grid-cols-1 gap-4 lg:grid-cols-3"
      data-testid="agent-editor"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate();
      }}
    >
      <div className="flex flex-col gap-4 lg:col-span-2">
        <Section title="Agent">
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <Field
              label="Name"
              htmlFor="agent-name"
              hint={`Becomes the flow ${d.project || "agents"}/${d.name || "…"}: schedules, rules and backfills see it as any flow`}
            >
              <Input
                id="agent-name"
                value={d.name}
                onChange={(e) => set({ name: e.target.value })}
                required
                className="font-mono"
              />
            </Field>
            <Field label="Project · group" htmlFor="agent-project">
              <div className="flex gap-2">
                <Input
                  id="agent-project"
                  value={d.project}
                  onChange={(e) => set({ project: e.target.value })}
                  className="font-mono"
                />
                <Input
                  aria-label="Group"
                  placeholder="group"
                  value={d.group ?? ""}
                  onChange={(e) => set({ group: e.target.value })}
                  className="font-mono"
                />
              </div>
            </Field>
          </div>
          <Field label="Description" htmlFor="agent-description">
            <Input
              id="agent-description"
              value={d.description}
              onChange={(e) => set({ description: e.target.value })}
            />
          </Field>
        </Section>

        <Section title="Model">
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <Field
              label="Model id"
              htmlFor="agent-model"
              hint={
                cat
                  ? `Providers installed here: ${
                      cat.providers
                        .filter((p) => p.installed)
                        .map((p) => p.id)
                        .join(", ") || "none"
                    }`
                  : undefined
              }
            >
              <Input
                id="agent-model"
                value={d.model}
                onChange={(e) => set({ model: e.target.value })}
                list="agent-models"
                required
                className="font-mono"
              />
              <datalist id="agent-models">
                {["openai:gpt-5", "anthropic:claude-sonnet-5-5", "google-gla:gemini-3-pro"].map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
            </Field>
            <Field
              label="API key · from a secret variable"
              htmlFor="agent-key"
              hint="Exported to the provider for the run only; never stored with the agent"
            >
              <NativeSelect
                id="agent-key"
                value={d.api_key ?? ""}
                onChange={(e) => set({ api_key: e.target.value || null })}
              >
                <NativeSelectOption value="">— the server's environment —</NativeSelectOption>
                {secrets.map((s) => (
                  <NativeSelectOption key={s} value={s}>
                    {s}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            </Field>
          </div>
        </Section>

        <Section
          title="Instructions"
          hint="What the agent is for. {braces} in the instructions or the prompt become parameters of the flow."
        >
          <Textarea
            id="agent-instructions"
            aria-label="Instructions"
            rows={5}
            value={d.instructions}
            onChange={(e) => set({ instructions: e.target.value })}
          />
          <Field label="Prompt template · the run's prompt parameter" htmlFor="agent-prompt">
            <Input
              id="agent-prompt"
              value={d.prompt}
              onChange={(e) => set({ prompt: e.target.value })}
              className="font-mono"
            />
          </Field>
        </Section>

        <Section
          title="Output"
          hint="The output is the run's result and a markdown artifact, so a rule or a downstream flow can use it."
        >
          <div className="flex gap-4 text-sm">
            {(["text", "json"] as const).map((kind) => (
              <label key={kind} className="flex items-center gap-2">
                <input
                  type="radio"
                  name="output"
                  checked={d.output.kind === kind}
                  onChange={() => set({ output: { ...d.output, kind } })}
                />
                {kind === "text" ? "Text" : "JSON matching a schema"}
              </label>
            ))}
          </div>
          {d.output.kind === "json" ? (
            <Textarea
              aria-label="Output schema"
              rows={6}
              className="font-mono text-xs"
              value={schemaText}
              onChange={(e) => setSchemaText(e.target.value)}
            />
          ) : null}
        </Section>
      </div>

      <div className="flex flex-col gap-4">
        <Section
          title="Tools"
          hint="From the Pydantic AI catalogue on this server. Ticked tools are attached."
        >
          {!cat ? <div className="text-sm text-muted-foreground">Loading the catalogue</div> : null}
          {cat && !cat.installed ? (
            <div className="text-sm text-muted-foreground">
              No host has the pydantic-ai extra, so nothing can be attached.
            </div>
          ) : null}
          {cat?.installed ? (
            <>
              <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                Capabilities
              </div>
              {cat.capabilities.map((c) => (
                <ToolRow
                  key={c.name}
                  id={`capability-${c.name}`}
                  checked={hasTool("capability", c.name)}
                  onChange={(on) => toggleTool({ kind: "capability", name: c.name, options: {} }, on)}
                  name={c.name}
                  description={c.description}
                  needs={c.needs}
                />
              ))}
              <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                Common tools
              </div>
              {cat.common.map((c) => (
                <ToolRow
                  key={c.name}
                  id={`common-${c.name}`}
                  checked={hasTool("common", c.name)}
                  onChange={(on) => toggleTool({ kind: "common", name: c.name, options: {} }, on)}
                  name={c.name}
                  description={c.description}
                  needs={c.needs}
                  disabled={!c.ready}
                />
              ))}
              <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                MCP servers
              </div>
              <ToolRow
                id="self-cereyan"
                checked={Boolean(selfTool)}
                onChange={(on) =>
                  on
                    ? toggleTool({ kind: "self", name: "cereyan", options: { tools: selfNames } }, true)
                    : toggleTool({ kind: "self", name: "cereyan", options: {} }, false)
                }
                name="cereyan"
                tags={["this server"]}
                description="The server's own MCP tools"
                needs={cat.self.ready ? [] : ['"self" in [agents] mcp_urls']}
                disabled={!cat.self.ready}
              >
                {selfTool ? (
                  <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1">
                    {cat.self.tools.map((t) => (
                      <label
                        key={t.name}
                        htmlFor={`self-tool-${t.name}`}
                        className="flex items-center gap-1.5 text-xs"
                      >
                        <Checkbox
                          id={`self-tool-${t.name}`}
                          checked={selfNames.includes(t.name)}
                          onCheckedChange={(v) => toggleSelfName(t.name, v === true)}
                        />
                        {t.name}
                        {!t.read_only ? (
                          <span className="rounded border px-1 text-[10px] text-muted-foreground">
                            writes
                          </span>
                        ) : null}
                      </label>
                    ))}
                  </div>
                ) : null}
              </ToolRow>
              {d.tools
                .filter((t) => t.kind === "mcp")
                .map((t) => (
                  <ToolRow
                    key={t.name}
                    id={`mcp-${t.name}`}
                    checked
                    onChange={() => toggleTool(t, false)}
                    name={t.name}
                    tags={[(t.options as { command?: string }).command ? "stdio" : "http"]}
                    description={
                      (t.options as { command?: string; url?: string }).command ??
                      (t.options as { url?: string }).url
                    }
                    needs={cat.mcp.ready ? [] : cat.mcp.needs}
                  />
                ))}
              <div
                className="flex flex-col gap-2 rounded-md border border-dashed p-2.5"
                data-testid="add-mcp"
              >
                <div className="text-xs font-medium">Add MCP server</div>
                <Input
                  aria-label="Server name"
                  placeholder="name"
                  value={mcpDraft.name}
                  onChange={(e) => setMcpDraft({ ...mcpDraft, name: e.target.value })}
                  className="font-mono"
                />
                <Input
                  aria-label="Server URL"
                  placeholder="https://… (allowed hosts only)"
                  value={mcpDraft.url}
                  onChange={(e) => setMcpDraft({ ...mcpDraft, url: e.target.value })}
                  className="font-mono"
                />
                <div className="flex gap-2">
                  <NativeSelect
                    aria-label="Command"
                    value={mcpDraft.command}
                    onChange={(e) => setMcpDraft({ ...mcpDraft, command: e.target.value })}
                  >
                    <NativeSelectOption value="">command…</NativeSelectOption>
                    {allowlist.mcp_commands.map((c) => (
                      <NativeSelectOption key={c} value={c}>
                        {c}
                      </NativeSelectOption>
                    ))}
                  </NativeSelect>
                  <Input
                    aria-label="Arguments"
                    placeholder="arguments"
                    value={mcpDraft.args}
                    onChange={(e) => setMcpDraft({ ...mcpDraft, args: e.target.value })}
                    className="font-mono"
                  />
                </div>
                <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
                  <span>
                    Commands from <code>[agents] mcp_commands</code> in cereyan.toml:{" "}
                    {allowlist.mcp_commands.join(", ") || "none"}. URLs must match <code>mcp_urls</code>.
                  </span>
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    onClick={addMcp}
                    disabled={!mcpDraft.name || (!mcpDraft.url && !mcpDraft.command)}
                  >
                    Add
                  </Button>
                </div>
              </div>
            </>
          ) : null}
        </Section>

        <Section title="Policy">
          <div className="text-xs text-muted-foreground">When a tool asks for approval</div>
          <div className="flex flex-wrap gap-3 text-sm">
            {(["ask", "approve", "deny"] as const).map((p) => (
              <label key={p} className="flex items-center gap-2">
                <input
                  type="radio"
                  name="approvals"
                  checked={d.policy.approvals === p}
                  onChange={() => set({ policy: { ...d.policy, approvals: p } })}
                />
                {p === "ask" ? "Ask in the inbox" : p === "approve" ? "Approve" : "Deny"}
              </label>
            ))}
          </div>
          <div className="text-xs text-muted-foreground">Run once, never silently twice</div>
          <div className="flex flex-wrap gap-x-3 gap-y-1">
            {toolNames.length === 0 ? (
              <span className="text-xs text-muted-foreground">attach a tool first</span>
            ) : null}
            {toolNames.map((name) => (
              <label key={name} htmlFor={`once-${name}`} className="flex items-center gap-1.5 text-xs">
                <Checkbox
                  id={`once-${name}`}
                  checked={d.policy.once.includes(name)}
                  onCheckedChange={(v) =>
                    set({
                      policy: {
                        ...d.policy,
                        once: v === true ? [...d.policy.once, name] : d.policy.once.filter((n) => n !== name),
                      },
                    })
                  }
                />
                {name}
              </label>
            ))}
          </div>
          <div className="grid grid-cols-2 gap-3">
            <Field label="Tokens per run" htmlFor="limit-tokens">
              <Input
                id="limit-tokens"
                type="number"
                min={0}
                value={d.limits.tokens ?? ""}
                onChange={(e) =>
                  set({ limits: { ...d.limits, tokens: e.target.value ? Number(e.target.value) : null } })
                }
              />
            </Field>
            <Field label="Model requests per run" htmlFor="limit-requests">
              <Input
                id="limit-requests"
                type="number"
                min={0}
                value={d.limits.requests ?? ""}
                onChange={(e) =>
                  set({ limits: { ...d.limits, requests: e.target.value ? Number(e.target.value) : null } })
                }
              />
            </Field>
            <Field label="Cost per run ($)" htmlFor="limit-cost">
              <Input
                id="limit-cost"
                type="number"
                min={0}
                step="0.01"
                value={d.limits.cost_usd ?? ""}
                onChange={(e) =>
                  set({ limits: { ...d.limits, cost_usd: e.target.value ? Number(e.target.value) : null } })
                }
              />
            </Field>
            <Field label="Timeout (s)" htmlFor="limit-timeout">
              <Input
                id="limit-timeout"
                type="number"
                min={0}
                value={d.limits.timeout_seconds ?? ""}
                onChange={(e) =>
                  set({
                    limits: { ...d.limits, timeout_seconds: e.target.value ? Number(e.target.value) : null },
                  })
                }
              />
            </Field>
            <Field label="Crash retries" htmlFor="crash-retries">
              <Input
                id="crash-retries"
                type="number"
                min={0}
                value={d.crash_retries}
                onChange={(e) => set({ crash_retries: Number(e.target.value) || 0 })}
              />
            </Field>
          </div>
          {allowlist.cost_per_day ? (
            <div className="text-xs text-muted-foreground">
              All agents together: at most ${allowlist.cost_per_day.toFixed(2)} a day ([agents] cost_per_day).
            </div>
          ) : null}
        </Section>

        {error ? (
          <div
            className="rounded-md border border-destructive/40 bg-destructive/5 p-3 text-sm text-destructive"
            role="alert"
            data-testid="agent-error"
          >
            {error}
          </div>
        ) : null}
        <div className="flex justify-end gap-2">
          <Button
            type="button"
            variant="outline"
            onClick={() =>
              navigate({
                to: id != null ? "/agents/$agentId" : "/agents",
                params: id != null ? { agentId: String(id) } : undefined,
              } as any)
            }
          >
            Cancel
          </Button>
          <Button type="submit" disabled={save.isPending} data-testid="save-agent">
            {id != null ? "Save changes" : "Save agent"}
          </Button>
        </div>
      </div>
    </form>
  );
}
