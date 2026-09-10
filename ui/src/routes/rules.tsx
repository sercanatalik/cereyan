import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useState } from "react";
import { ApiError, api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { JsonView } from "@/components/ported/json-view";
import { RuleForm, summarizeDo, summarizeUnless, summarizeWhen } from "@/components/rule-form";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Modal } from "@/components/ui/modal";
import { Switch } from "@/components/ui/switch";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { UnderlineTabs } from "@/components/ui/underline-tabs";
import { formatTime } from "@/lib/utils";

type Rule = components["schemas"]["RuleRow"];
type Search = { new?: string; events?: string; flow?: string };

export const Route = createFileRoute("/rules")({
  validateSearch: (s: Record<string, unknown>): Search => ({
    new: typeof s.new === "string" ? s.new : undefined,
    events: typeof s.events === "string" ? s.events : undefined,
    flow: typeof s.flow === "string" ? s.flow : undefined,
  }),
  component: RulesPage,
});

function RulesPage() {
  const search = Route.useSearch();
  const client = useQueryClient();
  const [editing, setEditing] = useState<Rule | "new" | null>(search.new ? "new" : null);
  const [open, setOpen] = useState<Rule | null>(null);
  const [tab, setTab] = useState("rule");
  const [error, setError] = useState<string | null>(null);
  const [tested, setTested] = useState<any>(null);
  const rules = useQuery({ queryKey: ["rules"], queryFn: async () => unwrap(await api.GET("/api/rules")) });
  const firings = useQuery({
    queryKey: ["firings", open?.id],
    queryFn: async () =>
      unwrap(await api.GET("/api/rules/{id}/firings", { params: { path: { id: open?.id as number } } })),
    enabled: !!open,
  });
  const invalidate = () => client.invalidateQueries({ queryKey: ["rules"] });
  const save = useMutation({
    mutationFn: async (body: any) => {
      const r =
        editing !== "new" && editing
          ? await api.PATCH("/api/rules/{id}", { params: { path: { id: editing.id } }, body })
          : await api.POST("/api/rules", { body });
      return unwrap(r as any);
    },
    onSuccess: () => {
      setEditing(null);
      setError(null);
      invalidate();
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
  });
  const toggle = useMutation({
    mutationFn: async ({ id, enabled }: { id: number; enabled: boolean }) =>
      unwrap(await api.PATCH("/api/rules/{id}", { params: { path: { id } }, body: { enabled } as any })),
    onSuccess: invalidate,
  });
  const remove = useMutation({
    mutationFn: async (id: number) => api.DELETE("/api/rules/{id}", { params: { path: { id } } }),
    onSuccess: () => {
      setOpen(null);
      invalidate();
    },
  });
  const test = useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.POST("/api/rules/{id}/test", { params: { path: { id } } })),
    onSuccess: (data) => setTested(data),
  });
  const initial =
    editing === "new"
      ? {
          name: search.events ? `on ${search.events}` : "",
          enabled: true,
          when: {
            events: search.events ? [search.events] : ["run.failed"],
            flows: search.flow ? [search.flow] : [],
            tags: [],
            states: [],
            project: null,
          },
          do: [{ kind: "webhook", url: "", body: "", parameters: {}, headers: {}, to: [] } as any],
          once: "per_run",
          cooldown_seconds: 0,
          max_per_minute: 60,
          allow_self: false,
        }
      : editing
        ? {
            name: editing.name,
            enabled: editing.enabled,
            when: editing.when,
            do: editing.do,
            once: editing.once,
            cooldown_seconds: editing.cooldown_seconds,
            max_per_minute: editing.max_per_minute,
            allow_self: editing.allow_self,
          }
        : undefined;
  return (
    <Page
      crumbs={[{ label: "Rules" }]}
      title="Rules"
      actions={
        <Button
          size="sm"
          onClick={() => {
            setEditing("new");
            setError(null);
          }}
        >
          New rule
        </Button>
      }
    >
      <div className="rounded-md border bg-card">
        <Table>
          <thead>
            <tr>
              <Th>Enabled</Th>
              <Th>Name</Th>
              <Th>When</Th>
              <Th>Do</Th>
              <Th>Last fired</Th>
              <Th>Count</Th>
            </tr>
          </thead>
          <tbody>
            {(rules.data ?? []).map((r) => (
              <Tr key={r.id} data-rule={r.name}>
                <Td>
                  <Switch
                    checked={r.enabled}
                    aria-label={`Enable ${r.name}`}
                    onCheckedChange={(enabled) => toggle.mutate({ id: r.id, enabled })}
                  />
                </Td>
                <Td>
                  <button
                    type="button"
                    className="font-medium hover:underline"
                    onClick={() => {
                      setOpen(r);
                      setTab("rule");
                      setTested(null);
                    }}
                  >
                    {r.name}
                  </button>
                  {r.source === "code" ? (
                    <span className="ml-2 inline-flex h-5 items-center rounded-[5px] border bg-muted px-1.5 text-[11.5px] font-medium">
                      code · {r.module}
                    </span>
                  ) : null}
                </Td>
                <Td className="text-xs text-muted-foreground">
                  {summarizeWhen(r.when)}
                  {r.unless ? (
                    <div className="text-amber-600 dark:text-amber-400">{summarizeUnless(r)}</div>
                  ) : null}
                </Td>
                <Td className="text-xs text-muted-foreground">{summarizeDo(r.do)}</Td>
                <Td>
                  {r.fire_count === 0 ? (
                    // Validation catches a misspelled name; nothing catches a
                    // rule that is spelled plausibly and matches nothing. This
                    // does.
                    <span
                      className="text-xs italic text-muted-foreground"
                      data-testid="rule-never-fired"
                      title="This rule has not matched an event yet"
                    >
                      never fired
                    </span>
                  ) : (
                    formatTime(r.last_fired)
                  )}
                </Td>
                <Td>{r.fire_count}</Td>
              </Tr>
            ))}
            {(rules.data ?? []).length === 0 ? (
              <tr>
                <td colSpan={6} className="px-3 py-8 text-center text-muted-foreground">
                  No rules yet
                </td>
              </tr>
            ) : null}
          </tbody>
        </Table>
      </div>
      <Modal
        open={editing !== null}
        onClose={() => setEditing(null)}
        title={editing === "new" ? "New rule" : `Edit ${editing?.name}`}
      >
        {editing ? (
          <RuleForm
            key={editing === "new" ? "new" : editing.id}
            initial={initial as any}
            onSave={(rule) => save.mutate(rule)}
            onCancel={() => setEditing(null)}
            busy={save.isPending}
            error={error}
          />
        ) : null}
      </Modal>
      <Modal open={open !== null} onClose={() => setOpen(null)} title={open?.name ?? ""}>
        {open ? (
          <div className="space-y-3">
            <UnderlineTabs
              items={[
                { value: "rule", label: "Rule" },
                { value: "firings", label: `Firings (${firings.data?.length ?? 0})` },
              ]}
              value={tab}
              onChange={setTab}
            />
            {tab === "rule" ? (
              <div className="space-y-2 text-sm">
                <div>
                  <span className="text-muted-foreground">When: </span>
                  {summarizeWhen(open.when)}
                </div>
                {open.unless ? (
                  <div>
                    <span className="text-muted-foreground">Unless: </span>
                    {summarizeUnless(open)}
                  </div>
                ) : null}
                <div>
                  <span className="text-muted-foreground">Do: </span>
                  {summarizeDo(open.do)}
                </div>
                {open.unless && !open.at ? <OpenExpectations ruleId={open.id} /> : null}
                <div className="text-xs text-muted-foreground">
                  once {open.once} · cooldown {open.cooldown_seconds}s · max {open.max_per_minute}/min
                  {open.allow_self ? " · allow self" : ""}
                  {open.source === "code" ? ` · code rule from ${open.module}` : ""}
                </div>
                <div className="flex gap-2">
                  <Button size="sm" variant="outline" onClick={() => test.mutate(open.id)}>
                    Test
                  </Button>
                  {open.source !== "code" ? (
                    <>
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => {
                          setEditing(open);
                          setOpen(null);
                        }}
                      >
                        Edit
                      </Button>
                      <Button
                        size="sm"
                        variant="destructive"
                        onClick={() => window.confirm("Delete this rule?") && remove.mutate(open.id)}
                      >
                        Delete
                      </Button>
                    </>
                  ) : null}
                </div>
                {tested ? <JsonView value={tested} /> : null}
              </div>
            ) : (
              <div className="max-h-96 space-y-2 overflow-auto text-sm">
                {(firings.data ?? []).map((f) => (
                  <div key={f.id} className="rounded border p-2">
                    <div className="text-xs text-muted-foreground">
                      {formatTime(f.timestamp)} · event {f.event_id ?? "-"} · run {f.run_id ?? "-"}
                    </div>
                    <ul className="mt-1 text-xs">
                      {(f.outcomes as any[]).map((o, i) => (
                        <li
                          key={`${f.id}-${o.kind}-${o.index ?? i}`}
                          className={o.status === "failed" ? "text-red-600" : ""}
                        >
                          {o.kind}: {o.status}
                          {o.error ? ` (${o.error})` : ""}
                        </li>
                      ))}
                    </ul>
                  </div>
                ))}
                {(firings.data ?? []).length === 0 ? (
                  <div className="text-muted-foreground">No firings yet</div>
                ) : null}
              </div>
            )}
          </div>
        ) : null}
      </Modal>
    </Page>
  );
}

/** Armed expectations of an event-armed rule, with their deadlines. */
function OpenExpectations({ ruleId }: { ruleId: number }) {
  const query = useQuery({
    queryKey: ["rules", ruleId, "expectations"],
    queryFn: async () =>
      unwrap(await api.GET("/api/rules/{id}/expectations", { params: { path: { id: ruleId }, query: {} } })),
    refetchInterval: 5_000,
  });
  const items = query.data ?? [];
  return (
    <div className="text-xs text-muted-foreground" data-testid="open-expectations">
      <span>Open expectations: {items.length}</span>
      {items.slice(0, 10).map((e) => (
        <div key={e.id}>
          {e.run_id ? (
            <Link to="/runs/$runId" params={{ runId: String(e.run_id) }} className="hover:underline">
              run {e.run_id}
            </Link>
          ) : (
            <span>{e.key}</span>
          )}{" "}
          · deadline {formatTime(e.deadline)}
        </div>
      ))}
    </div>
  );
}
