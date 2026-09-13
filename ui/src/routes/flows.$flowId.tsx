import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { CalendarClock, Clock, SkipForward, Undo2 } from "lucide-react";
import { marked } from "marked";
import { Fragment, useState } from "react";
import {
  ApiError,
  api,
  isUpcomingRun,
  type ScheduleRow,
  type StateType,
  type UpcomingItem,
  unwrap,
} from "@/api/client";
import { BackfillDialog } from "@/components/backfill-dialog";
import { JsonView } from "@/components/ported/json-view";
import { StateBadge, StateDot } from "@/components/ported/state-badge";
import { RescheduleDialog } from "@/components/reschedule-dialog";
import { RunForm } from "@/components/run-form";
import { RunTable } from "@/components/run-table";
import { describeSchedule, rowToDraft, ScheduleEditor, scheduleParts } from "@/components/schedule-editor";
import { Page } from "@/components/shell";
import { SkipDialog, upcomingQuery } from "@/components/skip-dialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Modal } from "@/components/ui/modal";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { UnderlineTabs } from "@/components/ui/underline-tabs";
import { cn, formatFire, formatIn, formatStamp, formatTime, relativeTime } from "@/lib/utils";

export const Route = createFileRoute("/flows/$flowId")({ component: FlowDetail });

/** Fires past the look-ahead the Upcoming tab fetches, and how many it adds at a time. */
const PROJECTED_FETCHED = 40;
const PROJECTED_STEP = 10;

type Fire = { sid: number; time: number };

function fireKey(f: Fire): string {
  return `${f.sid}:${f.time}`;
}

/** Who skipped a fire and when, as the Schedule column says it. */
function skipNote(item: UpcomingItem): string {
  const by =
    item.skipped_by === "ui" ? "in the UI" : item.skipped_by === "mcp" ? "by an agent" : "through the API";
  return item.skipped_at ? `Skipped ${by} · ${formatStamp(item.skipped_at)}` : `Skipped ${by}`;
}

/** A skip still to come: dashed, because the run has not ended yet. */
function SkippedBadge() {
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full border border-dashed border-muted-foreground/50 px-[7px] py-px text-xs font-medium text-muted-foreground">
      <SkipForward className="size-3" />
      Skipped
    </span>
  );
}

function ScheduleChip({ s }: { s: ScheduleRow }) {
  const { what, zone } = scheduleParts(s);
  return (
    <span
      className="inline-flex h-7 items-center gap-2 rounded-md border bg-card px-2.5 text-xs"
      data-testid="schedule-summary"
    >
      <Clock className="size-3.5 text-muted-foreground" />
      <span>{what}</span>
      {zone ? <span className="text-muted-foreground">{zone}</span> : null}
      <span className="text-muted-foreground">·</span>
      <span className="text-muted-foreground">
        {s.active
          ? `next ${s.next_fire ? formatFire(s.next_fire) : "-"}`
          : `paused${s.paused_reason ? ` (${s.paused_reason})` : ""}`}
      </span>
      {s.skipped ? (
        <span className="inline-flex h-[18px] items-center rounded-full bg-muted px-1.5 text-[11px] font-medium">
          {s.skipped} skipped
        </span>
      ) : null}
    </span>
  );
}

function FlowDetail() {
  const { flowId } = Route.useParams();
  const id = Number(flowId);
  const client = useQueryClient();
  const navigate = useNavigate();
  const [tab, setTab] = useState("runs");
  const [runOpen, setRunOpen] = useState(false);
  const [backfillOpen, setBackfillOpen] = useState(false);
  const [skipOpen, setSkipOpen] = useState(false);
  const [rescheduleOpen, setRescheduleOpen] = useState(false);
  const [editing, setEditing] = useState<ScheduleRow | "new" | null>(null);
  const [selected, setSelected] = useState<Map<string, Fire>>(new Map());
  const [projectedShown, setProjectedShown] = useState(PROJECTED_STEP);
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
  const upcoming = useQuery({ ...upcomingQuery(id, PROJECTED_FETCHED), refetchInterval: 15_000 });
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
  const skipFires = useMutation({
    mutationFn: async (fires: Fire[]) => {
      // The skip endpoints are per schedule: one request for each schedule involved.
      const bySchedule = new Map<number, number[]>();
      for (const f of fires) bySchedule.set(f.sid, [...(bySchedule.get(f.sid) ?? []), f.time]);
      for (const [sid, times] of bySchedule) {
        unwrap(
          await api.POST("/api/schedules/{sid}/skips", {
            params: { path: { sid } },
            body: { fires: times.sort((a, b) => a - b), by: "ui" },
          }),
        );
      }
    },
    onSuccess: () => {
      setSelected(new Map());
      invalidate();
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
  });
  const unskipFire = useMutation({
    mutationFn: async ({ sid, time }: Fire) =>
      unwrap(
        await api.DELETE("/api/schedules/{sid}/skips/{fire}", { params: { path: { sid, fire: time } } }),
      ),
    onSuccess: invalidate,
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
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
  const items = upcoming.data ?? [];
  const materialized = items.filter(isUpcomingRun);
  const projected = items.filter((i) => !isUpcomingRun(i));
  const shown: UpcomingItem[] = [...materialized, ...projected.slice(0, projectedShown)].sort(
    (a, b) => (a.scheduled_time ?? 0) - (b.scheduled_time ?? 0),
  );
  const firstProjected = shown.findIndex((i) => !isUpcomingRun(i));
  // A divider only reads right when every run sits above it.
  const divider = firstProjected > 0 && shown.slice(firstProjected).every((i) => !isUpcomingRun(i));
  const selectable: Fire[] = shown
    .filter((i) => i.schedule_id != null && i.scheduled_time != null && !i.skipped)
    .map((i) => ({ sid: i.schedule_id as number, time: i.scheduled_time as number }));
  const allSelected = selectable.length > 0 && selectable.every((fr) => selected.has(fireKey(fr)));
  const toggleFire = (fire: Fire, on: boolean) =>
    setSelected((prev) => {
      const next = new Map(prev);
      if (on) next.set(fireKey(fire), fire);
      else next.delete(fireKey(fire));
      return next;
    });
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
          {f.schedules.length ? (
            <Button size="sm" variant="outline" onClick={() => setSkipOpen(true)}>
              <SkipForward /> Skip next…
            </Button>
          ) : null}
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
      {f.schedules.length ? (
        <div className="flex flex-wrap items-center gap-2">
          {f.schedules.map((s) => (
            <ScheduleChip key={s.id} s={s} />
          ))}
          <Button size="xs" variant="ghost" onClick={() => setRescheduleOpen(true)}>
            <CalendarClock /> Reschedule
          </Button>
        </div>
      ) : null}
      <UnderlineTabs
        items={[
          { value: "runs", label: "Runs" },
          { value: "upcoming", label: `Upcoming (${materialized.length})` },
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
        <div className="overflow-hidden rounded-md border bg-card" data-testid="upcoming-card">
          <div
            className={cn(
              "flex min-h-11 items-center gap-3 border-b px-3 py-2",
              selected.size && "bg-muted/50",
            )}
          >
            {selected.size ? (
              <>
                <span className="text-xs font-medium">{selected.size} selected</span>
                <Button
                  size="xs"
                  variant="outline"
                  disabled={skipFires.isPending}
                  onClick={() => skipFires.mutate([...selected.values()])}
                >
                  <SkipForward /> Skip {selected.size === 1 ? "run" : `${selected.size} runs`}
                </Button>
                <Button size="xs" variant="ghost" onClick={() => setSelected(new Map())}>
                  Clear
                </Button>
                <span className="ml-auto text-xs text-muted-foreground">
                  A skipped run never starts. Undo it any time before it passes.
                </span>
              </>
            ) : (
              <span className="text-xs text-muted-foreground">
                Select upcoming runs to skip several at once.
              </span>
            )}
            {f.schedules.length ? (
              <Button
                size="xs"
                variant="outline"
                className={cn(!selected.size && "ml-auto")}
                onClick={() => setSkipOpen(true)}
              >
                <SkipForward /> Skip next…
              </Button>
            ) : null}
          </div>
          {error ? <div className="border-b px-3 py-2 text-xs text-red-600">{error}</div> : null}
          <Table>
            <thead>
              <tr>
                <Th className="w-10">
                  {selectable.length ? (
                    <Checkbox
                      checked={allSelected ? true : selected.size ? "indeterminate" : false}
                      onCheckedChange={(c) =>
                        setSelected(
                          c === true ? new Map(selectable.map((fr) => [fireKey(fr), fr])) : new Map(),
                        )
                      }
                      aria-label="Select all upcoming runs"
                    />
                  ) : null}
                </Th>
                <Th>Scheduled for</Th>
                <Th>Run</Th>
                <Th>State</Th>
                <Th>Schedule</Th>
                <Th className="w-28" />
              </tr>
            </thead>
            <tbody>
              {shown.map((item, index) => {
                const time = item.scheduled_time ?? 0;
                const sid = item.schedule_id ?? null;
                const fire = sid === null ? null : { sid, time };
                const schedule = f.schedules.find((s) => s.id === sid);
                return (
                  <Fragment key={fire ? fireKey(fire) : `run-${isUpcomingRun(item) ? item.id : index}`}>
                    {divider && index === firstProjected ? (
                      <tr data-testid="look-ahead-divider">
                        <td
                          colSpan={6}
                          className="border-b bg-muted/40 px-3 py-1.5 text-xs text-muted-foreground"
                        >
                          Beyond the look-ahead · projected from the schedule, created when they come due
                        </td>
                      </tr>
                    ) : null}
                    <Tr
                      data-skipped={item.skipped}
                      className={cn("group", fire && selected.has(fireKey(fire)) && "bg-muted")}
                    >
                      <Td>
                        {fire && !item.skipped ? (
                          <Checkbox
                            checked={selected.has(fireKey(fire))}
                            onCheckedChange={(c) => toggleFire(fire, c === true)}
                            aria-label={`Select ${formatFire(time)}`}
                          />
                        ) : null}
                      </Td>
                      <Td>
                        {item.skipped ? (
                          <span className="text-muted-foreground line-through">{formatFire(time)}</span>
                        ) : (
                          <>
                            <span>{formatFire(time)}</span>
                            <span className="ml-2 text-xs text-muted-foreground">{formatIn(time)}</span>
                          </>
                        )}
                      </Td>
                      <Td>
                        {isUpcomingRun(item) ? (
                          <Link
                            to="/runs/$runId"
                            params={{ runId: String(item.id) }}
                            className="hover:underline"
                          >
                            {item.name}
                          </Link>
                        ) : (
                          <span className="text-muted-foreground">not created yet</span>
                        )}
                      </Td>
                      <Td>
                        {item.skipped ? (
                          <SkippedBadge />
                        ) : isUpcomingRun(item) ? (
                          <StateBadge state={item.state} />
                        ) : (
                          <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
                            <span className="size-[7px] rounded-full border-[1.5px] border-amber-400" />
                            Projected
                          </span>
                        )}
                      </Td>
                      <Td className="text-muted-foreground">
                        {item.skipped ? (
                          <span className="text-xs">{skipNote(item)}</span>
                        ) : schedule ? (
                          scheduleParts(schedule).what
                        ) : (
                          "-"
                        )}
                      </Td>
                      <Td className="text-right">
                        {fire === null ? null : item.skipped ? (
                          <Button
                            size="xs"
                            variant="ghost"
                            disabled={unskipFire.isPending}
                            onClick={() => unskipFire.mutate(fire)}
                          >
                            <Undo2 /> Undo
                          </Button>
                        ) : (
                          <Button
                            size="xs"
                            variant="ghost"
                            className="opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
                            disabled={skipFires.isPending}
                            onClick={() => skipFires.mutate([fire])}
                          >
                            <SkipForward /> Skip
                          </Button>
                        )}
                      </Td>
                    </Tr>
                  </Fragment>
                );
              })}
              {shown.length === 0 ? (
                <tr>
                  <td colSpan={6} className="px-3 py-8 text-center text-muted-foreground">
                    No upcoming runs
                  </td>
                </tr>
              ) : null}
            </tbody>
          </Table>
          {projected.length > projectedShown ? (
            <div className="flex h-10 items-center border-t px-3">
              <Button
                size="xs"
                variant="ghost"
                className="text-muted-foreground"
                onClick={() => setProjectedShown((n) => n + PROJECTED_STEP)}
              >
                Show {PROJECTED_STEP} more
              </Button>
            </div>
          ) : null}
        </div>
      ) : null}
      {tab === "schedules" ? (
        <div className="space-y-3">
          {f.schedules.map((s) => (
            <div key={s.id} className="rounded-md border bg-card p-3">
              {editing !== "new" && editing?.id === s.id ? (
                <div className="space-y-3">
                  {s.source === "code" ? (
                    <div className="text-xs text-muted-foreground">
                      Declared in the flow's code: an edit lasts until the server restarts, then the
                      declaration applies again.
                    </div>
                  ) : null}
                  <ScheduleEditor
                    initial={rowToDraft(s)}
                    active={s.active}
                    saving={saveSchedule.isPending}
                    onSave={async (body) => {
                      await saveSchedule.mutateAsync({ sid: s.id, body });
                    }}
                    onToggle={(active) => toggleSchedule.mutate({ sid: s.id, active })}
                    onCancel={() => setEditing(null)}
                  />
                </div>
              ) : (
                <div className="flex items-center justify-between gap-2 text-sm">
                  <div>
                    <div>{describeSchedule(s)}</div>
                    <div className="text-xs text-muted-foreground">
                      {s.source} · catch-up {s.catchup} ·{" "}
                      {s.active
                        ? `next ${s.next_fire ? formatTime(s.next_fire) : "-"}`
                        : `paused${s.paused_reason ? ` (${s.paused_reason})` : ""}`}
                      {s.skipped ? ` · ${s.skipped} skipped` : ""}
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
      {skipOpen ? <SkipDialog flow={f} open onClose={() => setSkipOpen(false)} /> : null}
      {rescheduleOpen ? <RescheduleDialog flow={f} open onClose={() => setRescheduleOpen(false)} /> : null}
    </Page>
  );
}
