import cronstrue from "cronstrue";
import { useEffect, useState } from "react";
import { api, type ScheduleRow } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input, Select } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { UnderlineTabs } from "@/components/ui/underline-tabs";
import { formatTime } from "@/lib/utils";

/** Catch-up window, jitter, and start deadline in seconds; null or 0 is off. */
type Policies = { catchup_window: number | null; jitter: number; start_deadline: number | null };

export type ScheduleDraft =
  | ({
      kind: "cron";
      cron: string;
      timezone: string;
      day_or: boolean;
      catchup: string;
      catchup_max: number;
    } & Policies)
  | ({
      kind: "interval";
      interval: number;
      timezone: string;
      catchup: string;
      catchup_max: number;
    } & Policies)
  | ({ kind: "rrule"; rrule: string; timezone: string; catchup: string; catchup_max: number } & Policies)
  | ({
      kind: "continuous";
      /** Seconds from the end of one run to the next joining the line. */
      delay: number;
      timezone: string;
      catchup: string;
      catchup_max: number;
    } & Policies);

/** A flow's `disable_after`: failures, window in seconds (null for in a row), pause in seconds. */
export type DisableAfter = [number, number | null, number];

/** Seconds in their largest whole unit: "30 min", "2 h", "45 s". */
export function formatSeconds(seconds: number): string {
  if (seconds >= 86_400 && seconds % 86_400 === 0) return `${seconds / 86_400} d`;
  if (seconds >= 3600 && seconds % 3600 === 0) return `${seconds / 3600} h`;
  if (seconds >= 60 && seconds % 60 === 0) return `${seconds / 60} min`;
  return `${seconds} s`;
}

/** What a flow's `disable_after` does, in one sentence. */
export function describeDisableAfter(d: DisableAfter | null | undefined): string {
  if (!d) return "The flow sets no disable_after, so the loop keeps going after failures.";
  const [count, window, persist] = d;
  const when =
    window == null ? `${count} failures in a row` : `${count} failures within ${formatSeconds(window)}`;
  return `This flow pauses the loop after ${when} and resumes it ${formatSeconds(persist)} later.`;
}

export const TIMEZONES = [
  "local",
  "UTC",
  "Europe/Istanbul",
  "Europe/London",
  "Europe/Berlin",
  "Europe/Paris",
  "America/New_York",
  "America/Chicago",
  "America/Los_Angeles",
  "Asia/Tokyo",
  "Asia/Singapore",
  "Australia/Sydney",
];

export function cronDescription(cron: string): string | null {
  try {
    return cronstrue.toString(cron, { use24HourTimeFormat: true });
  } catch {
    return null;
  }
}

export function describeSchedule(s: ScheduleRow): string {
  const sc = s.schedule as any;
  if (sc.kind === "cron") return `${cronDescription(sc.cron) ?? sc.cron} (${sc.timezone ?? "local"})`;
  if (sc.kind === "interval")
    return `every ${sc.interval >= 86400 ? `${sc.interval / 86400} d` : sc.interval >= 3600 ? `${sc.interval / 3600} h` : `${sc.interval} s`}`;
  if (sc.kind === "continuous")
    return sc.delay > 0 ? `continuous, ${formatSeconds(sc.delay)} after each run` : "continuous";
  return `rrule ${String(sc.rrule).split("\n").pop()}`;
}

/** A schedule's words and its timezone, for places that show them apart. */
export function scheduleParts(s: ScheduleRow): { what: string; zone: string | null } {
  const sc = s.schedule as any;
  if (sc.kind === "cron") return { what: cronDescription(sc.cron) ?? sc.cron, zone: sc.timezone ?? "local" };
  return { what: describeSchedule(s), zone: sc.timezone ?? null };
}

export function rowToDraft(s: ScheduleRow): ScheduleDraft {
  const sc = s.schedule as any;
  const common = {
    timezone: sc.timezone ?? "local",
    catchup: s.catchup,
    catchup_max: s.catchup_max,
    catchup_window: s.catchup_window ?? null,
    jitter: s.jitter ?? 0,
    start_deadline: s.start_deadline ?? null,
  };
  if (sc.kind === "cron") return { kind: "cron", cron: sc.cron, day_or: sc.day_or ?? true, ...common };
  if (sc.kind === "interval") return { kind: "interval", interval: sc.interval, ...common };
  if (sc.kind === "continuous") return { kind: "continuous", delay: sc.delay ?? 0, ...common };
  return { kind: "rrule", rrule: sc.rrule, ...common };
}

export function draftToBody(d: ScheduleDraft): Record<string, unknown> {
  const tz = d.timezone === "local" ? null : d.timezone;
  const base = {
    catchup: d.catchup,
    catchup_max: d.catchup_max,
    timezone: tz,
    catchup_window: d.catchup_window ?? 0,
    jitter: d.jitter ?? 0,
    start_deadline: d.start_deadline ?? 0,
  };
  if (d.kind === "cron") return { kind: "cron", cron: d.cron, day_or: d.day_or, ...base };
  if (d.kind === "interval") return { kind: "interval", interval: d.interval, ...base };
  if (d.kind === "continuous")
    // No fire times, so no zone and nothing to catch up.
    return { kind: "continuous", delay: d.delay, jitter: base.jitter, start_deadline: base.start_deadline };
  return { kind: "rrule", rrule: d.rrule, ...base };
}

const UNITS = [
  { label: "seconds", seconds: 1 },
  { label: "minutes", seconds: 60 },
  { label: "hours", seconds: 3600 },
] as const;

/** The largest unit that divides `seconds` evenly, for the wait input. */
function unitFor(seconds: number): number {
  for (const u of [...UNITS].reverse()) if (seconds > 0 && seconds % u.seconds === 0) return u.seconds;
  return 1;
}

/** Runs alternating with waits: the shape of a continuous schedule. */
function LoopStrip() {
  const run = "fill-sky-500";
  const line = "fill-amber-400";
  return (
    <div className="space-y-2 rounded-md border p-3" data-testid="loop-preview">
      <svg
        viewBox="0 0 560 22"
        className="h-6 w-full"
        role="img"
        aria-label="Runs alternate with waits that hold no processor"
      >
        <rect x="0" y="4" width="70" height="14" rx="3" className={run} />
        <rect x="70" y="10" width="130" height="2" className="fill-border" />
        <rect x="200" y="4" width="22" height="14" rx="3" className={line} />
        <rect x="222" y="4" width="58" height="14" rx="3" className={run} />
        <rect x="280" y="10" width="130" height="2" className="fill-border" />
        <rect x="410" y="4" width="40" height="14" rx="3" className={line} />
        <rect x="450" y="4" width="80" height="14" rx="3" className={run} />
      </svg>
      <div className="flex flex-wrap gap-4 text-xs text-muted-foreground">
        <span className="flex items-center gap-1.5">
          <span className="size-2.5 rounded-sm bg-sky-500" />
          Running, holds a processor
        </span>
        <span className="flex items-center gap-1.5">
          <span className="size-2.5 rounded-sm bg-amber-400" />
          In line, time varies with load
        </span>
        <span className="flex items-center gap-1.5">
          <span className="h-0.5 w-2.5 bg-border" />
          Waiting, holds nothing
        </span>
      </div>
      <p className="text-xs text-muted-foreground">
        Unlike an interval, runs never overlap: the next one joins the line only after the last one ends. At
        most one run of this schedule is waiting or in line at a time.
      </p>
    </div>
  );
}

export async function previewSchedule(
  body: Record<string, unknown>,
  count = 3,
): Promise<{ next: number[]; error?: string }> {
  const r = await api.POST("/api/schedules/preview", { body: { ...body, count } as any });
  if (r.response.ok && r.data) return { next: r.data.next };
  return { next: [], error: (r.error as any)?.error ?? "invalid schedule" };
}

export function ScheduleEditor({
  initial,
  active,
  onSave,
  onToggle,
  onCancel,
  saving,
  preview = previewSchedule,
  disableAfter,
}: {
  initial?: ScheduleDraft;
  /** The flow's `disable_after`, shown read-only on the Continuous tab. */
  disableAfter?: DisableAfter | null;
  active?: boolean;
  onSave: (body: Record<string, unknown>) => Promise<void> | void;
  onToggle?: (active: boolean) => void;
  onCancel?: () => void;
  saving?: boolean;
  preview?: (body: Record<string, unknown>) => Promise<{ next: number[]; error?: string }>;
}) {
  const [draft, setDraft] = useState<ScheduleDraft>(
    initial ?? {
      kind: "cron",
      cron: "0 9 * * *",
      timezone: "local",
      day_or: true,
      catchup: "skip",
      catchup_max: 100,
      catchup_window: null,
      jitter: 0,
      start_deadline: null,
    },
  );
  const [error, setError] = useState<string | null>(null);
  const [next, setNext] = useState<number[]>([]);
  const description = draft.kind === "cron" ? cronDescription(draft.cron) : null;

  useEffect(() => {
    let cancelled = false;
    const body = draftToBody(draft);
    if (draft.kind === "cron" && !cronDescription(draft.cron)) {
      setNext([]);
      return;
    }
    preview(body)
      .then((r) => {
        if (cancelled) return;
        setNext(r.next);
        setError(r.error ?? null);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [draft, preview]);

  const setKind = (kind: string) => {
    const common = {
      timezone: draft.timezone,
      catchup: draft.catchup,
      catchup_max: draft.catchup_max,
      catchup_window: draft.catchup_window,
      jitter: draft.jitter,
      start_deadline: draft.start_deadline,
    };
    if (kind === "cron") setDraft({ kind: "cron", cron: "0 9 * * *", day_or: true, ...common });
    else if (kind === "interval") setDraft({ kind: "interval", interval: 3600, ...common });
    else if (kind === "continuous") setDraft({ kind: "continuous", delay: 1800, ...common });
    else
      setDraft({ kind: "rrule", rrule: "DTSTART:20260101T090000Z\nRRULE:FREQ=WEEKLY;BYDAY=MO", ...common });
  };
  const invalid =
    draft.kind === "cron"
      ? !description
      : draft.kind === "interval"
        ? !(draft.interval > 0)
        : draft.kind === "continuous"
          ? !(draft.delay >= 0)
          : !draft.rrule.includes("DTSTART");
  const submit = async () => {
    if (invalid) {
      setError(
        draft.kind === "cron"
          ? "Invalid cron expression"
          : draft.kind === "interval"
            ? "Interval must be positive"
            : draft.kind === "continuous"
              ? "The wait must be zero or more"
              : "RRule must include DTSTART",
      );
      return;
    }
    try {
      await onSave(draftToBody(draft));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="space-y-3" data-testid="schedule-editor">
      <UnderlineTabs
        items={[
          { value: "cron", label: "Cron" },
          { value: "interval", label: "Interval" },
          { value: "rrule", label: "RRule" },
          { value: "continuous", label: "Continuous" },
        ]}
        value={draft.kind}
        onChange={setKind}
      />
      {draft.kind === "cron" ? (
        <div className="space-y-1">
          <label className="text-xs text-muted-foreground" htmlFor="cron-input">
            Cron expression
          </label>
          <Input
            id="cron-input"
            value={draft.cron}
            onChange={(e) => setDraft({ ...draft, cron: e.target.value })}
            className="font-mono"
          />
          <div className="text-xs" data-testid="cron-preview">
            {description ?? <span className="text-red-600">Invalid cron expression</span>}
          </div>
          <span className="flex items-center gap-2 text-xs text-muted-foreground">
            <Checkbox
              checked={draft.day_or}
              onCheckedChange={(c) => setDraft({ ...draft, day_or: c === true })}
              aria-label="Day-of-month OR day-of-week"
            />
            Day-of-month OR day-of-week (Vixie semantics)
          </span>
        </div>
      ) : null}
      {draft.kind === "interval" ? (
        <div className="space-y-1">
          <label className="text-xs text-muted-foreground" htmlFor="interval-input">
            Interval (seconds)
          </label>
          <Input
            id="interval-input"
            type="number"
            min={1}
            value={draft.interval}
            onChange={(e) => setDraft({ ...draft, interval: Number(e.target.value) })}
          />
          <div className="text-xs text-muted-foreground">
            {draft.interval >= 86400
              ? "Wall-clock interval (keeps local time across DST)"
              : "Elapsed-time interval"}
          </div>
        </div>
      ) : null}
      {draft.kind === "rrule" ? (
        <div className="space-y-1">
          <label className="text-xs text-muted-foreground" htmlFor="rrule-input">
            RRule (with DTSTART)
          </label>
          <textarea
            id="rrule-input"
            className="w-full rounded-md border bg-card px-2 py-1 font-mono text-xs"
            rows={3}
            value={draft.rrule}
            onChange={(e) => setDraft({ ...draft, rrule: e.target.value })}
          />
        </div>
      ) : null}
      {draft.kind === "continuous" ? (
        <ContinuousFields
          delay={draft.delay}
          onDelay={(delay) => setDraft({ ...draft, delay })}
          disableAfter={disableAfter}
        />
      ) : null}
      <div className="grid grid-cols-3 gap-2">
        {draft.kind === "continuous" ? null : (
          <>
            <label className="text-xs text-muted-foreground" htmlFor="sched-tz">
              Timezone
              <Select
                id="sched-tz"
                value={draft.timezone}
                onChange={(e) => setDraft({ ...draft, timezone: e.target.value })}
                className="mt-1 w-full"
              >
                {TIMEZONES.map((t) => (
                  <option key={t} value={t}>
                    {t}
                  </option>
                ))}
              </Select>
            </label>
            <label className="text-xs text-muted-foreground" htmlFor="sched-catchup">
              Catch-up
              <Select
                id="sched-catchup"
                value={draft.catchup}
                onChange={(e) => setDraft({ ...draft, catchup: e.target.value })}
                className="mt-1 w-full"
              >
                <option value="skip">skip missed fires</option>
                <option value="latest">run the latest missed</option>
                <option value="all">run all missed</option>
              </Select>
            </label>
            <label className="text-xs text-muted-foreground" htmlFor="sched-max">
              Catch-up max
              <Input
                id="sched-max"
                type="number"
                min={1}
                className="mt-1"
                value={draft.catchup_max}
                onChange={(e) => setDraft({ ...draft, catchup_max: Number(e.target.value) })}
              />
            </label>
            <label className="text-xs text-muted-foreground" htmlFor="sched-window">
              Catch-up window (s)
              <Input
                id="sched-window"
                type="number"
                min={0}
                className="mt-1"
                placeholder="off"
                value={draft.catchup_window ?? ""}
                onChange={(e) =>
                  setDraft({
                    ...draft,
                    catchup_window: e.target.value === "" ? null : Number(e.target.value),
                  })
                }
              />
            </label>
          </>
        )}
        <label className="text-xs text-muted-foreground" htmlFor="sched-jitter">
          Jitter (s)
          <Input
            id="sched-jitter"
            type="number"
            min={0}
            className="mt-1"
            value={draft.jitter}
            onChange={(e) => setDraft({ ...draft, jitter: Number(e.target.value) || 0 })}
          />
        </label>
        <label className="text-xs text-muted-foreground" htmlFor="sched-deadline">
          Start deadline (s)
          <Input
            id="sched-deadline"
            type="number"
            min={0}
            className="mt-1"
            placeholder="off"
            value={draft.start_deadline ?? ""}
            onChange={(e) =>
              setDraft({ ...draft, start_deadline: e.target.value === "" ? null : Number(e.target.value) })
            }
          />
        </label>
      </div>
      {onToggle ? (
        <span className="flex items-center gap-2 text-sm">
          <Switch checked={!active} onCheckedChange={(paused) => onToggle(!paused)} aria-label="Paused" />{" "}
          Paused
        </span>
      ) : null}
      <div className="text-xs text-muted-foreground" data-testid="next-fires">
        {draft.kind === "continuous" ? (
          `Runs again ${formatSeconds(draft.delay)} after each run ends, when a processor is free`
        ) : next.length ? (
          <>Next: {next.map((n) => formatTime(n)).join(", ")}</>
        ) : (
          "No upcoming fire times"
        )}
      </div>
      {error ? <div className="text-xs text-red-600">{error}</div> : null}
      <div className="flex justify-end gap-2">
        {onCancel ? (
          <Button variant="outline" size="sm" onClick={onCancel}>
            Cancel
          </Button>
        ) : null}
        <Button size="sm" onClick={submit} disabled={saving}>
          Save schedule
        </Button>
      </div>
    </div>
  );
}

function ContinuousFields({
  delay,
  onDelay,
  disableAfter,
}: {
  delay: number;
  onDelay: (seconds: number) => void;
  disableAfter?: DisableAfter | null;
}) {
  const [unit, setUnit] = useState(() => unitFor(delay));
  const code = `schedule=Continuous(delay=${delay === 0 ? 0 : JSON.stringify(formatSeconds(delay).replace(" min", "m").replace(" ", ""))})`;
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-end gap-2">
        <label className="text-xs text-muted-foreground" htmlFor="delay-input">
          Wait after each run
          <Input
            id="delay-input"
            type="number"
            min={0}
            className="mt-1 w-28 font-mono"
            value={delay / unit}
            onChange={(e) => onDelay(Math.max(0, Number(e.target.value) || 0) * unit)}
          />
        </label>
        <Select
          aria-label="Unit"
          value={unit}
          onChange={(e) => {
            const next = Number(e.target.value);
            onDelay((delay / unit) * next);
            setUnit(next);
          }}
          className="w-32"
        >
          {UNITS.map((u) => (
            <option key={u.seconds} value={u.seconds}>
              {u.label}
            </option>
          ))}
        </Select>
        <span className="pb-2 text-xs text-muted-foreground">
          Counted from the moment a run ends. 0 rejoins the line at once.
        </span>
      </div>
      <div className="rounded-md border bg-muted/40 px-3 py-2 text-xs" data-testid="disable-after-note">
        <div className="font-medium text-foreground">Stop on repeated failure</div>
        <div className="text-muted-foreground">{describeDisableAfter(disableAfter)}</div>
        {disableAfter ? null : (
          <div className="mt-1 font-mono text-muted-foreground">
            Add disable_after=(3, None, 6 * 3600) to the flow to pause it after three failures in a row.
          </div>
        )}
      </div>
      <LoopStrip />
      <pre className="overflow-x-auto rounded-md bg-muted px-3 py-2 font-mono text-xs">{code}</pre>
    </div>
  );
}
