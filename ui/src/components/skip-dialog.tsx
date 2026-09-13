import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowRight, CornerDownRight, Minus, Plus } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { api, type Flow, type ScheduleRow, unwrap } from "@/api/client";
import { describeSchedule } from "@/components/schedule-editor";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input, Select } from "@/components/ui/input";
import { Modal } from "@/components/ui/modal";
import { Segmented } from "@/components/ui/segmented";
import { cn, formatFire, formatIn } from "@/lib/utils";

/** How many fires past the look-ahead the skip and reschedule views ask for. */
export const PROJECTED = 20;

/** The schedule that fires next, or the first one when none is active. */
export function nextSchedule(flow: Flow): ScheduleRow | undefined {
  const active = flow.schedules
    .filter((s) => s.active && s.next_fire != null)
    .sort((a, b) => (a.next_fire ?? 0) - (b.next_fire ?? 0));
  return active[0] ?? flow.schedules[0];
}

/** Flows that run after `flow`, directly or further down its chain, by name. */
export function downstreamOf(flow: Flow, flows: Flow[]): string[] {
  const out: string[] = [];
  const frontier = [...flow.triggers];
  while (frontier.length) {
    const name = frontier.shift();
    if (name === undefined) break;
    if (name === flow.name || out.includes(name)) continue;
    out.push(name);
    const next = flows.find((f) => f.project === flow.project && f.name === name);
    if (next) frontier.push(...next.triggers);
  }
  return out;
}

export function upcomingQuery(flowId: number, projected = PROJECTED) {
  return {
    queryKey: ["upcoming", flowId, "projected", projected],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/flows/{id}/upcoming", {
          params: { path: { id: flowId }, query: { projected } },
        }),
      ),
  };
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function downstreamLine(flow: Flow, names: string[], count: number): string {
  const runs = plural(count, "run");
  if (names.length === 1)
    return `The ${names[0]} flow runs after ${flow.name}, so ${runs} of ${names[0]} ${count === 1 ? "is" : "are"} skipped too.`;
  return `${names.join(", ")} run after ${flow.name}, so ${runs} of each ${count === 1 ? "is" : "are"} skipped too.`;
}

export function SkipDialog({ flow, open, onClose }: { flow: Flow; open: boolean; onClose: () => void }) {
  const client = useQueryClient();
  const [sid, setSid] = useState<number | undefined>(() => nextSchedule(flow)?.id);
  const [mode, setMode] = useState("next");
  const [count, setCount] = useState(1);
  const [until, setUntil] = useState("");
  const [ticked, setTicked] = useState<Set<number>>(new Set());
  const upcoming = useQuery({ ...upcomingQuery(flow.id), enabled: open });
  const flows = useQuery({
    queryKey: ["flows"],
    queryFn: async () => unwrap(await api.GET("/api/flows")),
    enabled: open,
  });
  const fires = useMemo(
    () =>
      (upcoming.data ?? [])
        .filter((i) => i.schedule_id === sid && i.scheduled_time != null)
        .slice(0, 14)
        .map((i) => ({ time: i.scheduled_time as number, skipped: i.skipped })),
    [upcoming.data, sid],
  );
  const openFires = fires.filter((f) => !f.skipped);
  // The stepper and the until field fill the checklist; ticking by hand refines it.
  useEffect(() => {
    const available = fires.filter((f) => !f.skipped);
    if (mode === "next") {
      setTicked(new Set(available.slice(0, count).map((f) => f.time)));
    } else {
      const limit = Date.parse(until) * 1000;
      setTicked(
        new Set(Number.isNaN(limit) ? [] : available.filter((f) => f.time < limit).map((f) => f.time)),
      );
    }
  }, [mode, count, until, fires]);
  const resumes = fires.find((f) => !f.skipped && !ticked.has(f.time));
  const downstream = downstreamOf(flow, flows.data ?? []);
  const submit = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/schedules/{sid}/skips", {
          params: { path: { sid: sid as number } },
          body: { fires: [...ticked].sort((a, b) => a - b), by: "ui" },
        }),
      ),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["flows"] });
      client.invalidateQueries({ queryKey: ["upcoming"] });
      client.invalidateQueries({ queryKey: ["runs"] });
      onClose();
    },
  });
  const toggle = (time: number, on: boolean) =>
    setTicked((prev) => {
      const next = new Set(prev);
      if (on) next.add(time);
      else next.delete(time);
      return next;
    });
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Skip upcoming runs"
      description={`${flow.name}: the schedule stays on. A skipped fire never starts; its run ends Skipped at its time.`}
      className="sm:max-w-md"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button
            disabled={ticked.size === 0 || sid === undefined || submit.isPending}
            onClick={() => submit.mutate()}
          >
            Skip {plural(ticked.size, "run")}
          </Button>
        </>
      }
    >
      <div className="space-y-3.5" data-testid="skip-dialog">
        <label className="block text-xs text-muted-foreground" htmlFor="skip-schedule">
          Schedule
          <Select
            id="skip-schedule"
            value={sid}
            onChange={(e) => setSid(Number(e.target.value))}
            disabled={flow.schedules.length < 2}
            className="mt-1.5 w-full"
          >
            {flow.schedules.map((s) => (
              <option key={s.id} value={s.id}>
                {describeSchedule(s)}
              </option>
            ))}
          </Select>
        </label>
        <Segmented
          label="Skip by"
          items={[
            { value: "next", label: "Next runs" },
            { value: "until", label: "Until a time" },
          ]}
          value={mode}
          onChange={setMode}
        />
        {mode === "next" ? (
          <div className="flex items-center gap-2.5 text-sm">
            Skip the next
            <span className="inline-flex h-8 overflow-hidden rounded-md border shadow-xs">
              <Button
                variant="ghost"
                size="icon-sm"
                className="h-full w-8 rounded-none border-r"
                aria-label="Fewer"
                onClick={() => setCount((c) => Math.max(1, c - 1))}
              >
                <Minus />
              </Button>
              <span
                className="grid w-11 place-content-center font-medium tabular-nums"
                data-testid="skip-count"
              >
                {count}
              </span>
              <Button
                variant="ghost"
                size="icon-sm"
                className="h-full w-8 rounded-none border-l"
                aria-label="More"
                onClick={() => setCount((c) => Math.min(Math.max(1, openFires.length), c + 1))}
              >
                <Plus />
              </Button>
            </span>
            {count === 1 ? "run" : "runs"}
          </div>
        ) : (
          <label className="block text-xs text-muted-foreground" htmlFor="skip-until">
            Every scheduled run before this time is skipped.
            <Input
              id="skip-until"
              type="datetime-local"
              value={until}
              onChange={(e) => setUntil(e.target.value)}
              className="mt-1.5"
            />
          </label>
        )}
        <div className="max-h-64 overflow-y-auto rounded-md border">
          {fires.map((f) => (
            <div
              key={f.time}
              className="flex h-8 items-center gap-2 border-b px-2.5 text-xs last:border-b-0"
              data-testid="skip-fire"
            >
              <Checkbox
                id={`skip-fire-${f.time}`}
                checked={f.skipped || ticked.has(f.time)}
                disabled={f.skipped}
                onCheckedChange={(c) => toggle(f.time, c === true)}
              />
              <label
                htmlFor={`skip-fire-${f.time}`}
                className={cn(f.skipped && "text-muted-foreground line-through")}
              >
                {formatFire(f.time)}
              </label>
              <span className="ml-auto text-muted-foreground">
                {f.skipped ? "already skipped" : formatIn(f.time)}
              </span>
            </div>
          ))}
          {fires.length === 0 ? (
            <div className="px-3 py-6 text-center text-sm text-muted-foreground">
              {upcoming.isLoading ? "Loading" : "No upcoming fires"}
            </div>
          ) : null}
        </div>
        <div className="space-y-1.5 text-xs">
          <div className="flex items-center gap-2" data-testid="skip-next-run">
            <ArrowRight className="size-3 shrink-0 text-muted-foreground" />
            {resumes ? (
              <span>
                Schedule resumes <span className="font-medium">{formatFire(resumes.time)}</span>
              </span>
            ) : (
              <span>No run left in this list</span>
            )}
          </div>
          {downstream.length && ticked.size ? (
            <div className="flex items-start gap-2 text-muted-foreground" data-testid="skip-downstream">
              <CornerDownRight className="mt-0.5 size-3 shrink-0" />
              <span>{downstreamLine(flow, downstream, ticked.size)}</span>
            </div>
          ) : null}
        </div>
        {submit.error ? <div className="text-xs text-red-600">{submit.error.message}</div> : null}
      </div>
    </Modal>
  );
}
