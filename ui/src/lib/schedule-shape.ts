/**
 * The cron shape the Reschedule dialog edits as fields: a fixed minute and
 * hour on a list of weekdays, `M H * * D`. Anything else stays raw cron.
 */
export interface CronShape {
  minute: number;
  hour: number;
  /** Weekdays, 0 = Sunday through 6 = Saturday, ascending; all seven for `*`. */
  days: number[];
  /** Day of the month for a monthly schedule, `M H D * *`; absent otherwise. */
  monthDay?: number;
}

export const WEEKDAYS = [
  { value: 1, label: "Mon" },
  { value: 2, label: "Tue" },
  { value: 3, label: "Wed" },
  { value: 4, label: "Thu" },
  { value: 5, label: "Fri" },
  { value: 6, label: "Sat" },
  { value: 0, label: "Sun" },
];

function number(text: string, max: number): number | null {
  if (!/^\d{1,2}$/.test(text)) return null;
  const n = Number(text);
  return n <= max ? n : null;
}

function weekdays(text: string): number[] | null {
  if (text === "*") return [0, 1, 2, 3, 4, 5, 6];
  const out = new Set<number>();
  for (const part of text.split(",")) {
    const m = /^(\d)(?:-(\d))?$/.exec(part);
    if (!m) return null;
    const from = Number(m[1]);
    const to = m[2] === undefined ? from : Number(m[2]);
    if (from > 7 || to > 7 || to < from) return null;
    // Cron allows 7 for Sunday as well as 0.
    for (let d = from; d <= to; d++) out.add(d % 7);
  }
  return [...out].sort((a, b) => a - b);
}

export function parseCronShape(cron: string): CronShape | null {
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 5 || parts[3] !== "*") return null;
  const minute = number(parts[0], 59);
  const hour = number(parts[1], 23);
  if (parts[2] !== "*") {
    // Monthly: a day of the month, every weekday.
    const monthDay = number(parts[2], 31);
    if (minute === null || hour === null || monthDay === null || monthDay < 1 || parts[4] !== "*")
      return null;
    return { minute, hour, days: [0, 1, 2, 3, 4, 5, 6], monthDay };
  }
  const days = weekdays(parts[4]);
  if (minute === null || hour === null || !days || days.length === 0) return null;
  return { minute, hour, days };
}

export function formatCronShape({ minute, hour, days, monthDay }: CronShape): string {
  if (monthDay !== undefined) return `${minute} ${hour} ${monthDay} * *`;
  const set = [...new Set(days.map((d) => d % 7))].sort((a, b) => a - b);
  if (set.length === 7) return `${minute} ${hour} * * *`;
  // Runs of three or more consecutive days read as a range: 1,2,3,4,5 is 1-5.
  const parts: string[] = [];
  let i = 0;
  while (i < set.length) {
    let j = i;
    while (j + 1 < set.length && set[j + 1] === set[j] + 1) j++;
    parts.push(j - i >= 2 ? `${set[i]}-${set[j]}` : set.slice(i, j + 1).join(","));
    i = j + 1;
  }
  return `${minute} ${hour} * * ${parts.join(",")}`;
}

export function clockOf({ minute, hour }: Pick<CronShape, "minute" | "hour">): string {
  return `${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}`;
}

/** Midnight of today and the six days after it, in the viewer's timezone. */
export function weekFrom(now: Date): Date[] {
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  return Array.from(
    { length: 7 },
    (_, i) => new Date(start.getFullYear(), start.getMonth(), start.getDate() + i),
  );
}

/** The fire times among `fires` (microseconds) that fall on `day`. */
export function firesOn(fires: number[], day: Date): number[] {
  return fires.filter((f) => {
    const d = new Date(f / 1000);
    return (
      d.getFullYear() === day.getFullYear() &&
      d.getMonth() === day.getMonth() &&
      d.getDate() === day.getDate()
    );
  });
}
