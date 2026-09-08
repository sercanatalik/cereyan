import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { marked } from "marked";
import { useState } from "react";
import { ApiError, api, type Flow, type StateType, unwrap } from "@/api/client";
import { BackfillDialog } from "@/components/backfill-dialog";
import { JsonView } from "@/components/ported/json-view";
import { StateDot } from "@/components/ported/state-badge";
import { RunForm } from "@/components/run-form";
import { RunTable } from "@/components/run-table";
import { cronDescription, type ScheduleDraft, ScheduleEditor } from "@/components/schedule-editor";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Modal } from "@/components/ui/modal";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { UnderlineTabs } from "@/components/ui/underline-tabs";
import { formatTime, relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/flows/$flowId")({ component: FlowDetail });

type ScheduleRow = Flow["schedules"][number];

export function describeSchedule(s: ScheduleRow): string {
  const sc = s.schedule as any;
  if (sc.kind === "cron") return `${cronDescription(sc.cron) ?? sc.cron} (${sc.timezone ?? "local"})`;
  if (sc.kind === "interval")
    return `every ${sc.interval >= 86400 ? `${sc.interval / 86400} d` : sc.interval >= 3600 ? `${sc.interval / 3600} h` : `${sc.interval} s`}`;
  return `rrule ${String(sc.rrule).split("\n").pop()}`;
}

function toDraft(s: ScheduleRow): ScheduleDraft {
  const sc = s.schedule as any;
  const common = { timezone: sc.timezone ?? "local", catchup: s.catchup, catchup_max: s.catchup_max };
  if (sc.kind === "cron") return { kind: "cron", cron: sc.cron, day_or: sc.day_or ?? true, ...common };
  if (sc.kind === "interval") return { kind: "interval", interval: sc.interval, ...common };
  return { kind: "rrule", rrule: sc.rrule, ...common };
}

function FlowDetail() {
  const { flowId } = Route.useParams();
  const id = Number(flowId);
  const client = useQueryClient();
  const navigate = useNavigate();
  const [tab, setTab] = useState("runs");
  const [runOpen, setRunOpen] = useState(false);
  const [backfillOpen, setBackfillOpen] = useState(false);
  const [editing, setEditing] = useState<ScheduleRow | "new" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const flow = useQuery({
    queryKey: ["flows", id],
    queryFn: async () => unwrap(await api.GET("/api/flows/{id}", { params: { path: { id } } })),
  });
  const runs = useQuery({
    queryKey: ["runs", "flow", id],
    queryFn: async () =>
      unwrap(await api.GET("/api/runs", { params: { query: { flow_id: id, limit: 50 } } })),
  });
  const upcoming = useQuery({
    queryKey: ["upcoming", id],
    queryFn: async () => unwrap(await api.GET("/api/flows/{id}/upcoming", { params: { path: { id } } })),
    refetchInterval: 15_000,
  });
  const invalidate = () => {
    client.invalidateQueries({ queryKey: ["flows"] });
    client.invalidateQueries({ queryKey: ["upcoming", id] });
    client.invalidateQueries({ queryKey: ["runs"] });
  };
  const run = useMutation({
    mutationFn: async (parameters: Record<string, unknown>) =>
      unwrap(
        await api.POST("/api/flows/{id}/runs", {
          params: { path: { id } },
          body: { parameters: parameters as any, tags: [] },
        }),
      ),
    onSuccess: (created) => {
      setRunOpen(false);
      navigate({ to: "/runs/$runId", params: { runId: String(created.id) } });
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
  });
  const backfill = useMutation({
    mutationFn: async (body: any) =>
      unwrap(await api.POST("/api/flows/{id}/backfill", { params: { path: { id } }, body })),
    onSuccess: (status) => {
      setBackfillOpen(false);
      navigate({ to: "/runs", search: { tab: "runs", tag: status.tag } as any });
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
  });
  const pauseAll = useMutation({
    mutationFn: async (pause: boolean) =>
      pause
        ? api.POST("/api/flows/{id}/pause", { params: { path: { id } } })
        : api.POST("/api/flows/{id}/resume", { params: { path: { id } } }),
    onSuccess: invalidate,
  });
  const saveSchedule = useMutation({
    mutationFn: async ({ sid, body }: { sid?: number; body: Record<string, unknown> }) => {
      const r: any = sid
        ? await api.PATCH("/api/schedules/{sid}", { params: { path: { sid } }, body: body as any })
        : await api.POST("/api/flows/{id}/schedules", { params: { path: { id } }, body: body as any });
      return unwrap(r) as unknown;
    },
    onSuccess: () => {
      setEditing(null);
      invalidate();
    },
  });
  const toggleSchedule = useMutation({
    mutationFn: async ({ sid, active }: { sid: number; active: boolean }) =>
      active
        ? api.POST("/api/schedules/{sid}/resume", { params: { path: { sid } } })
        : api.POST("/api/schedules/{sid}/pause", { params: { path: { sid } } }),
    onSuccess: invalidate,
  });
  const removeSchedule = useMutation({
    mutationFn: async (sid: number) => api.DELETE("/api/schedules/{sid}", { params: { path: { sid } } }),
    onSuccess: invalidate,
  });
  const f = flow.data;
  if (!f)
    return (
      <Page crumbs={[{ label: "Flows", to: "/flows" }, { label: flowId }]}>
        {flow.isError ? "Flow not found" : "Loading"}
      </Page>
    );
  const options = (f.options ?? {}) as any;
  const anyActive = f.schedules.some((s) => s.active);
  const description = f.description ? (marked.parse(f.description) as string) : "";
  return (
    <Page
      crumbs={[{ label: "Flows", to: "/flows" }, { label: f.name }]}
      actions={
        <div className="flex gap-2">
          <Button
            size="sm"
            onClick={() => {
              setError(null);
              setRunOpen(true);
            }}
            disabled={!f.live}
          >
            Run
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => {
              setError(null);
              setBackfillOpen(true);
            }}
            disabled={!f.live}
          >
            Backfill
          </Button>
          <Button
            size="sm"
            variant="outline"
            disabled={f.schedules.length === 0}
            onClick={() => pauseAll.mutate(anyActive)}
          >
            {anyActive ? "Pause" : "Resume"}
          </Button>
        </div>
      }
    >
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-lg font-semibold">{f.name}</h1>
        <span className="text-muted-foreground">{f.project}</span>
        <span className="flex gap-1">
          {f.recent_runs
            .slice()
            .reverse()
            .map(([rid, type, name]) => (
              <StateDot key={rid} type={type as StateType} title={`${name} (run ${rid})`} />
            ))}
        </span>
        <span className="inline-flex h-5 items-center rounded-[5px] border bg-muted px-1.5 text-[11.5px] font-medium">
          priority {options.priority ?? 0}
        </span>
        <span className="inline-flex h-5 items-center rounded-[5px] border bg-muted px-1.5 text-[11.5px] font-medium">
          max_concurrent {options.max_concurrent ?? "unlimited"}
        </span>
        <span className="inline-flex h-5 items-center rounded-[5px] border bg-muted px-1.5 text-[11.5px] font-medium">
          on_overlap {options.on_overlap ?? "enqueue"}
        </span>
        {f.triggered_by ? (
          <span className="text-xs text-muted-foreground">
            Triggered by {(f.upstreams?.length ? f.upstreams : [f.triggered_by]).join(" + ")}
            {f.batch_key ? ` per ${f.batch_key}` : ""}
          </span>
        ) : null}
        {f.triggers.length ? (
          <span className="text-xs text-muted-foreground">Triggers {f.triggers.join(", ")}</span>
        ) : null}
        {f.error ? <span className="text-xs text-red-600">{f.error}</span> : null}
        {!f.live ? (
          <span className="text-xs text-muted-foreground">
            not registered by this server, last seen {relativeTime(f.last_seen_at)}
          </span>
        ) : null}
      </div>
      {description ? (
        <div
          className="prose prose-sm max-w-none text-sm text-muted-foreground"
          // biome-ignore lint/security/noDangerouslySetInnerHtml: docstring markdown from the user's own code
          dangerouslySetInnerHTML={{ __html: description }}
        />
      ) : null}
      <div className="flex flex-wrap gap-2 text-xs text-muted-foreground">
        {f.schedules.map((s) => (
          <span key={s.id} className="rounded border px-2 py-0.5">
            {describeSchedule(s)}
            {s.active
              ? ` · next ${s.next_fire ? formatTime(s.next_fire) : "-"}`
              : ` · paused${s.paused_reason ? ` (${s.paused_reason})` : ""}`}
          </span>
        ))}
      </div>
      <UnderlineTabs
        items={[
          { value: "runs", label: "Runs" },
          { value: "upcoming", label: `Upcoming (${upcoming.data?.length ?? 0})` },
          { value: "schedules", label: `Schedules (${f.schedules.length})` },
          { value: "parameters", label: "Parameters" },
        ]}
        value={tab}
        onChange={setTab}
      />
      {tab === "runs" ? (
        <div className="rounded-md border bg-card">
          <RunTable runs={runs.data?.items ?? []} />
        </div>
      ) : null}
      {tab === "upcoming" ? (
        <div className="rounded-md border bg-card">
          <Table>
            <thead>
              <tr>
                <Th>Scheduled for</Th>
                <Th>Run</Th>
                <Th>State</Th>
                <Th>Schedule</Th>
              </tr>
            </thead>
            <tbody>
              {(upcoming.data ?? []).map((r) => (
                <Tr key={r.id}>
                  <Td>{formatTime(r.scheduled_time)}</Td>
                  <Td>
                    <Link to="/runs/$runId" params={{ runId: String(r.id) }} className="hover:underline">
                      {r.name}
                    </Link>
                  </Td>
                  <Td>{r.state.name}</Td>
                  <Td className="text-muted-foreground">{r.schedule_id ?? "-"}</Td>
                </Tr>
              ))}
              {(upcoming.data ?? []).length === 0 ? (
                <tr>
                  <td colSpan={4} className="px-3 py-8 text-center text-muted-foreground">
                    No upcoming runs
                  </td>
                </tr>
              ) : null}
            </tbody>
          </Table>
        </div>
      ) : null}
      {tab === "schedules" ? (
        <div className="space-y-3">
          {f.schedules.map((s) => (
            <div key={s.id} className="rounded-md border bg-card p-3">
              {editing !== "new" && editing?.id === s.id ? (
                <ScheduleEditor
                  initial={toDraft(s)}
                  active={s.active}
                  saving={saveSchedule.isPending}
                  onSave={async (body) => {
                    await saveSchedule.mutateAsync({ sid: s.id, body });
                  }}
                  onToggle={(active) => toggleSchedule.mutate({ sid: s.id, active })}
                  onCancel={() => setEditing(null)}
                />
              ) : (
                <div className="flex items-center justify-between gap-2 text-sm">
                  <div>
                    <div>{describeSchedule(s)}</div>
                    <div className="text-xs text-muted-foreground">
                      {s.source} · catch-up {s.catchup} ·{" "}
                      {s.active
                        ? `next ${s.next_fire ? formatTime(s.next_fire) : "-"}`
                        : `paused${s.paused_reason ? ` (${s.paused_reason})` : ""}`}
                      {s.persist ? " · persisted override" : ""}
                    </div>
                  </div>
                  <div className="flex gap-2">
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => toggleSchedule.mutate({ sid: s.id, active: !s.active })}
                    >
                      {s.active ? "Pause" : "Resume"}
                    </Button>
                    <Button size="sm" variant="outline" onClick={() => setEditing(s)}>
                      Edit
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => window.confirm("Delete this schedule?") && removeSchedule.mutate(s.id)}
                    >
                      Delete
                    </Button>
                  </div>
                </div>
              )}
            </div>
          ))}
          {editing === "new" ? (
            <div className="rounded-md border bg-card p-3">
              <ScheduleEditor
                saving={saveSchedule.isPending}
                onSave={async (body) => {
                  await saveSchedule.mutateAsync({ body });
                }}
                onCancel={() => setEditing(null)}
              />
            </div>
          ) : (
            <Button size="sm" variant="outline" onClick={() => setEditing("new")}>
              Add schedule
            </Button>
          )}
          {saveSchedule.isError ? (
            <div className="text-xs text-red-600">{String((saveSchedule.error as Error).message)}</div>
          ) : null}
        </div>
      ) : null}
      {tab === "parameters" ? <JsonView value={f.parameter_schema} /> : null}
      <Modal open={runOpen} onClose={() => setRunOpen(false)} title={`Run ${f.project}/${f.name}`}>
        <RunForm
          schema={f.parameter_schema}
          onSubmit={(values) => run.mutate(values)}
          busy={run.isPending}
          serverError={error}
        />
      </Modal>
      <BackfillDialog
        flow={f}
        open={backfillOpen}
        onClose={() => setBackfillOpen(false)}
        onSubmit={(body) => backfill.mutate(body)}
        busy={backfill.isPending}
        error={error}
      />
    </Page>
  );
}
