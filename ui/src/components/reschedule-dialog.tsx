import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, ChevronRight, FileCode2, RefreshCw, Undo2 } from "lucide-react";
import { useState } from "react";
import { ApiError, api, type Flow, isUpcomingRun, type ScheduleRow, unwrap } from "@/api/client";
import {
  cronDescription,
  describeSchedule,
  previewSchedule,
  rowToDraft,
  ScheduleEditor,
  scheduleParts,
  TIMEZONES,
} from "@/components/schedule-editor";
import { nextSchedule, upcomingQuery } from "@/components/skip-dialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input, Select } from "@/components/ui/input";
import { Modal } from "@/components/ui/modal";
import { Segmented } from "@/components/ui/segmented";
import {
  type CronShape,
  clockOf,
  firesOn,
  formatCronShape,
  parseCronShape,
  WEEKDAYS,
  weekFrom,
} from "@/lib/schedule-shape";
import { cn, formatClock, formatFire } from "@/lib/utils";

/** Enough fires to fill a week for a schedule that runs a few times a day. */
const PREVIEW_COUNT = 20;
const ALL_DAYS = [0, 1, 2, 3, 4, 5, 6];
const CATCHUP: Record<string, string> = {
  skip: "skip missed fires",
  latest: "run the latest missed",
  all: "run all missed",
};

type Repeats = "daily" | "weekly" | "monthly" | "cron";

function repeatsOf(shape: CronShape | null): Repeats {
  if (!shape) return "cron";
  if (shape.monthDay !== undefined) return "monthly";
  return shape.days.length === 7 ? "daily" : "weekly";
}

function nowLine(row: ScheduleRow): string {
  const { what, zone } = scheduleParts(row);
  return `Now: ${what}${zone ? ` · ${zone}` : ""}`;
}

export function RescheduleDialog({
  flow,
  open,
  onClose,
}: {
  flow: Flow;
  open: boolean;
  onClose: () => void;
}) {
  const [sid, setSid] = useState<number | undefined>(() => nextSchedule(flow)?.id);
  const row = flow.schedules.find((s) => s.id === sid);
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={`Reschedule ${flow.name}`}
      description={row ? nowLine(row) : undefined}
      className="sm:max-w-2xl"
    >
      <div className="space-y-4" data-testid="reschedule-dialog">
        {flow.schedules.length > 1 ? (
          <label className="block text-xs text-muted-foreground" htmlFor="reschedule-schedule">
            Schedule
            <Select
              id="reschedule-schedule"
              value={sid}
              onChange={(e) => setSid(Number(e.target.value))}
              className="mt-1.5 w-full"
            >
              {flow.schedules.map((s) => (
                <option key={s.id} value={s.id}>
                  {describeSchedule(s)}
                </option>
              ))}
            </Select>
          </label>
        ) : null}
        {row ? <ScheduleForm key={row.id} flow={flow} row={row} onClose={onClose} /> : null}
      </div>
    </Modal>
  );
}

function ScheduleForm({ flow, row, onClose }: { flow: Flow; row: ScheduleRow; onClose: () => void }) {
  const client = useQueryClient();
  const save = useMutation({
    mutationFn: async (body: Record<string, unknown>) =>
      unwrap(
        await api.PATCH("/api/schedules/{sid}", { params: { path: { sid: row.id } }, body: body as any }),
      ),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["flows"] });
      client.invalidateQueries({ queryKey: ["upcoming"] });
      client.invalidateQueries({ queryKey: ["runs"] });
      onClose();
    },
  });
  const note =
    row.source === "code" ? (
      <div className="flex gap-2.5 rounded-md border bg-muted p-3" data-testid="reschedule-code-note">
        <FileCode2 className="mt-0.5 size-4 shrink-0" />
        <div className="flex flex-col gap-0.5">
          <span className="text-sm font-medium">Declared in the flow's code</span>
          <span className="text-xs text-muted-foreground">
            Saving here lasts until the server restarts; then the declaration applies again.
          </span>
        </div>
      </div>
    ) : null;
  if ((row.schedule as any).kind !== "cron") {
    // Interval and RRule schedules keep the full editor.
    return (
      <div className="space-y-3">
        {note}
        <ScheduleEditor
          initial={rowToDraft(row)}
          saving={save.isPending}
          onSave={async (body) => {
            await save.mutateAsync(body);
          }}
          onCancel={onClose}
        />
      </div>
    );
  }
  return <CronForm flow={flow} row={row} note={note} save={save} onClose={onClose} />;
}

function Impact({ icon: Icon, children }: { icon: typeof RefreshCw; children: React.ReactNode }) {
  return (
    <div className="flex items-start gap-2">
      <Icon className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
      <span>{children}</span>
    </div>
  );
}

function CronForm({
  flow,
  row,
  note,
  save,
  onClose,
}: {
  flow: Flow;
  row: ScheduleRow;
  note: React.ReactNode;
  save: { mutate: (body: Record<string, unknown>) => void; isPending: boolean; error: Error | null };
  onClose: () => void;
}) {
  const current = row.schedule as any;
  const shape = parseCronShape(current.cron);
  const [repeats, setRepeats] = useState<Repeats>(repeatsOf(shape));
  const [text, setText] = useState<string>(current.cron);
  const [time, setTime] = useState(shape ? clockOf(shape) : "09:00");
  const [days, setDays] = useState<number[]>(shape && shape.days.length < 7 ? shape.days : [1, 2, 3, 4, 5]);
  const [monthDay, setMonthDay] = useState<number>(shape?.monthDay ?? 1);
  const [timezone, setTimezone] = useState<string>(current.timezone ?? "local");
  const [catchup, setCatchup] = useState<string>(row.catchup);
  const [dayOr, setDayOr] = useState<boolean>(current.day_or ?? true);
  const [optionsOpen, setOptionsOpen] = useState(false);
  const [hour, minute] = time.split(":").map(Number);
  const timeOk = Number.isInteger(hour) && Number.isInteger(minute);
  const fromFields = !timeOk
    ? null
    : repeats === "daily"
      ? formatCronShape({ minute, hour, days: ALL_DAYS })
      : repeats === "weekly"
        ? days.length
          ? formatCronShape({ minute, hour, days })
          : null
        : repeats === "monthly" && monthDay >= 1 && monthDay <= 31
          ? formatCronShape({ minute, hour, days: ALL_DAYS, monthDay })
          : null;
  const cron = repeats === "cron" ? text.trim() : fromFields;
  const description = cron ? cronDescription(cron) : null;
  const tz = timezone === "local" ? null : timezone;
  const timingChanged =
    cron !== current.cron || tz !== (current.timezone ?? null) || dayOr !== (current.day_or ?? true);
  const changed = timingChanged || catchup !== row.catchup;
  const before = useQuery({
    queryKey: ["preview", "cron", current.cron, current.timezone ?? null],
    queryFn: () =>
      previewSchedule(
        {
          kind: "cron",
          cron: current.cron,
          day_or: current.day_or ?? true,
          timezone: current.timezone ?? null,
        },
        PREVIEW_COUNT,
      ),
  });
  const after = useQuery({
    queryKey: ["preview", "cron", cron, tz, dayOr],
    queryFn: () => previewSchedule({ kind: "cron", cron, day_or: dayOr, timezone: tz }, PREVIEW_COUNT),
    enabled: description !== null,
  });
  const upcoming = useQuery(upcomingQuery(flow.id));
  const mine = (upcoming.data ?? []).filter((i) => i.schedule_id === row.id && i.scheduled_time != null);
  const skipped = mine.filter((i) => i.skipped).map((i) => i.scheduled_time as number);
  const replaced = mine.filter((i) => isUpcomingRun(i) && !i.skipped).length;
  const newFires = after.data?.next ?? [];
  const horizon = newFires.length ? newFires[newFires.length - 1] : 0;
  // Only skips within the previewed range can be judged; the server records the rest.
  const dropped =
    description && newFires.length ? skipped.filter((t) => t <= horizon && !newFires.includes(t)) : [];
  const toggleDay = (d: number) =>
    setDays((prev) => (prev.includes(d) ? prev.filter((x) => x !== d) : [...prev, d].sort((a, b) => a - b)));
  const choose = (next: string) => {
    if (next === "cron") {
      setText(cron ?? text);
      setRepeats("cron");
      return;
    }
    if (repeats === "cron") {
      // Carry what the raw cron said into the fields where it fits their shape.
      const parsed = parseCronShape(text);
      if (parsed) {
        setTime(clockOf(parsed));
        if (parsed.monthDay !== undefined) setMonthDay(parsed.monthDay);
        else if (parsed.days.length < 7) setDays(parsed.days);
      }
    }
    setRepeats(next as Repeats);
  };
  const zones = TIMEZONES.includes(timezone) ? TIMEZONES : [timezone, ...TIMEZONES];
  const week = weekFrom(new Date());
  const timezoneSelect = (
    <Select
      id="reschedule-tz"
      value={timezone}
      onChange={(e) => setTimezone(e.target.value)}
      className="w-full"
    >
      {zones.map((z) => (
        <option key={z} value={z}>
          {z}
        </option>
      ))}
    </Select>
  );
  const parsedText = repeats === "cron" ? parseCronShape(text) : null;
  return (
    <div className="space-y-4">
      {note}
      <div className="grid grid-cols-[88px_minmax(0,1fr)] items-center gap-x-4 gap-y-3">
        <span className="text-xs text-muted-foreground">Repeats</span>
        <Segmented
          label="Repeats"
          items={[
            { value: "daily", label: "Daily" },
            { value: "weekly", label: "Weekly" },
            { value: "monthly", label: "Monthly" },
            { value: "cron", label: "Cron" },
          ]}
          value={repeats}
          onChange={choose}
        />
        {repeats === "weekly" ? (
          <>
            <span className="text-xs text-muted-foreground">On</span>
            <fieldset className="m-0 flex flex-wrap gap-1.5 border-0 p-0" aria-label="Weekdays">
              {WEEKDAYS.map((d) => (
                <Button
                  key={d.value}
                  size="sm"
                  variant={days.includes(d.value) ? "default" : "outline"}
                  aria-pressed={days.includes(d.value)}
                  onClick={() => toggleDay(d.value)}
                  className="w-12"
                >
                  {d.label}
                </Button>
              ))}
            </fieldset>
          </>
        ) : null}
        {repeats === "monthly" ? (
          <>
            <label className="text-xs text-muted-foreground" htmlFor="reschedule-monthday">
              On day
            </label>
            <Input
              id="reschedule-monthday"
              type="number"
              min={1}
              max={31}
              value={monthDay}
              onChange={(e) => setMonthDay(Number(e.target.value))}
              className="w-24"
            />
          </>
        ) : null}
        {repeats === "cron" ? (
          <>
            <label className="text-xs text-muted-foreground" htmlFor="reschedule-tz">
              Timezone
            </label>
            {timezoneSelect}
          </>
        ) : (
          <>
            <span className="text-xs text-muted-foreground">At</span>
            <div className="flex items-center gap-3">
              <Input
                type="time"
                aria-label="Time"
                value={time}
                onChange={(e) => setTime(e.target.value)}
                className="w-28 shrink-0"
              />
              <label className="shrink-0 text-xs text-muted-foreground" htmlFor="reschedule-tz">
                Timezone
              </label>
              {timezoneSelect}
            </div>
          </>
        )}
        <span className="self-start pt-2 text-xs text-muted-foreground">Cron</span>
        <div className="space-y-1.5">
          {repeats === "cron" ? (
            <Input
              aria-label="Cron expression"
              value={text}
              onChange={(e) => setText(e.target.value)}
              className="font-mono"
            />
          ) : null}
          <div className="flex min-h-8 items-center gap-2.5 rounded-md bg-muted px-2.5 py-1.5 text-xs">
            <span className="font-mono" data-testid="reschedule-cron-text">
              {cron || "-"}
            </span>
            <span className={cn(description ? "text-muted-foreground" : "text-red-600")}>
              {description ??
                (repeats === "weekly" && days.length === 0
                  ? "Pick at least one day"
                  : "Invalid cron expression")}
            </span>
            {repeats === "cron" ? (
              parsedText ? (
                <Button
                  variant="link"
                  size="xs"
                  className="ml-auto"
                  onClick={() => choose(repeatsOf(parsedText))}
                >
                  Edit as fields
                </Button>
              ) : null
            ) : (
              <Button variant="link" size="xs" className="ml-auto" onClick={() => choose("cron")}>
                Edit as cron
              </Button>
            )}
          </div>
        </div>
      </div>
      <div className="space-y-2.5 rounded-md border bg-card p-3" data-testid="reschedule-week">
        <div className="flex items-center justify-between gap-3 text-xs">
          <span className="font-medium">
            Week of {week[0].toLocaleDateString(undefined, { day: "numeric", month: "short" })}
          </span>
          <span className="flex gap-3 text-muted-foreground">
            <span className="inline-flex items-center gap-1.5">
              <span className="size-2.5 rounded-[3px] border" />
              now
            </span>
            <span className="inline-flex items-center gap-1.5">
              <span className="size-2.5 rounded-[3px] bg-primary" />
              after saving
            </span>
            <span className="inline-flex items-center gap-1.5">
              <span className="size-2.5 rounded-[3px] border border-dashed border-muted-foreground/60" />
              skipped
            </span>
          </span>
        </div>
        <div className="grid grid-cols-[3.5rem_repeat(7,minmax(0,1fr))] items-start gap-1.5 text-[11.5px]">
          <span />
          {week.map((d) => (
            <span key={d.toISOString()} className="text-center text-muted-foreground">
              {d.toLocaleDateString(undefined, { weekday: "short", day: "numeric" })}
            </span>
          ))}
          <span className="pt-1 text-muted-foreground">Now</span>
          {week.map((d) => (
            <DayCell key={d.toISOString()} fires={firesOn(before.data?.next ?? [], d)} skipped={skipped} />
          ))}
          <span className="pt-1 text-muted-foreground">New</span>
          {week.map((d) => (
            <DayCell key={d.toISOString()} fires={firesOn(newFires, d)} skipped={skipped} fresh />
          ))}
        </div>
      </div>
      {changed && description ? (
        <div className="space-y-2 text-xs" data-testid="reschedule-impact">
          <div className="text-muted-foreground">When you save</div>
          {timingChanged ? (
            <Impact icon={RefreshCw}>
              {replaced === 1
                ? "1 scheduled run is replaced by a run at the new time."
                : `${replaced} scheduled runs are replaced by runs at the new times.`}
            </Impact>
          ) : null}
          {dropped.length ? (
            <div className="space-y-2" data-testid="reschedule-dropped">
              {dropped.map((t) => (
                <Impact key={t} icon={Undo2}>
                  The skip on {formatFire(t)} no longer matches a fire and is dropped.
                </Impact>
              ))}
            </div>
          ) : null}
          {catchup !== row.catchup ? (
            <Impact icon={RefreshCw}>
              After downtime, missed fires are handled as "{CATCHUP[catchup]}".
            </Impact>
          ) : null}
          {row.source === "code" ? (
            <Impact icon={FileCode2}>The flow's declaration applies again when the server restarts.</Impact>
          ) : null}
        </div>
      ) : null}
      <div className="border-t pt-3">
        <button
          type="button"
          className="flex w-full cursor-pointer items-center gap-1.5 text-xs"
          aria-expanded={optionsOpen}
          onClick={() => setOptionsOpen((o) => !o)}
        >
          {optionsOpen ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
          Catch-up and cron options
          <span className="ml-auto text-muted-foreground">
            {CATCHUP[catchup] ?? catchup} · {dayOr ? "day-of-month OR weekday" : "day-of-month AND weekday"}
          </span>
        </button>
        {optionsOpen ? (
          <div className="mt-3 grid grid-cols-[88px_minmax(0,1fr)] items-center gap-x-4 gap-y-3">
            <label className="text-xs text-muted-foreground" htmlFor="reschedule-catchup">
              Catch-up
            </label>
            <Select
              id="reschedule-catchup"
              value={catchup}
              onChange={(e) => setCatchup(e.target.value)}
              className="w-full"
            >
              {Object.entries(CATCHUP).map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </Select>
            <span className="text-xs text-muted-foreground">Days</span>
            <span className="flex items-center gap-2 text-xs">
              <Checkbox id="reschedule-dayor" checked={dayOr} onCheckedChange={(c) => setDayOr(c === true)} />
              <label htmlFor="reschedule-dayor">Day-of-month OR weekday (Vixie semantics)</label>
            </span>
          </div>
        ) : null}
      </div>
      {save.error ? (
        <div className="text-xs text-red-600">
          {save.error instanceof ApiError ? save.error.message : String(save.error)}
        </div>
      ) : null}
      <div className="flex justify-end gap-2">
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
        <Button
          disabled={!description || !changed || save.isPending}
          onClick={() =>
            save.mutate({
              cron,
              day_or: dayOr,
              ...(tz ? { timezone: tz } : {}),
              ...(catchup !== row.catchup ? { catchup } : {}),
            })
          }
        >
          Save schedule
        </Button>
      </div>
    </div>
  );
}

function DayCell({ fires, skipped, fresh }: { fires: number[]; skipped: number[]; fresh?: boolean }) {
  if (fires.length === 0) return <span className="pt-1 text-center text-muted-foreground/60">-</span>;
  const shown = fires.slice(0, 3);
  return (
    <span className="flex flex-col items-stretch gap-0.5">
      {shown.map((f) => {
        const isSkipped = skipped.includes(f);
        return (
          <span
            key={f}
            className={cn(
              "grid h-6 place-content-center rounded-[5px] font-medium tabular-nums",
              fresh ? "bg-primary text-primary-foreground" : "border bg-background",
              isSkipped &&
                "border border-dashed border-muted-foreground/60 bg-transparent text-muted-foreground line-through",
            )}
            title={isSkipped ? "skipped" : undefined}
          >
            {formatClock(f)}
          </span>
        );
      })}
      {fires.length > shown.length ? (
        <span className="text-center text-muted-foreground">+{fires.length - shown.length}</span>
      ) : null}
    </span>
  );
}
