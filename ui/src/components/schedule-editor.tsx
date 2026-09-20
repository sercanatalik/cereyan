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
  | ({ kind: "rrule"; rrule: string; timezone: string; catchup: string; catchup_max: number } & Policies);

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
  return { kind: "rrule", rrule: d.rrule, ...base };
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
}: {
  initial?: ScheduleDraft;
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
    else
      setDraft({ kind: "rrule", rrule: "DTSTART:20260101T090000Z\nRRULE:FREQ=WEEKLY;BYDAY=MO", ...common });
  };
  const invalid =
    draft.kind === "cron"
      ? !description
      : draft.kind === "interval"
        ? !(draft.interval > 0)
        : !draft.rrule.includes("DTSTART");
  const submit = async () => {
    if (invalid) {
      setError(
        draft.kind === "cron"
          ? "Invalid cron expression"
          : draft.kind === "interval"
            ? "Interval must be positive"
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
      <div className="grid grid-cols-3 gap-2">
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
              setDraft({ ...draft, catchup_window: e.target.value === "" ? null : Number(e.target.value) })
            }
          />
        </label>
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
        {next.length ? <>Next: {next.map((n) => formatTime(n)).join(", ")}</> : "No upcoming fire times"}
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
