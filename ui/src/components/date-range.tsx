import { FilterSelect } from "@/components/filter-select";

/** The time windows a list can be narrowed to, newest first. */
const PRESETS = [
  { key: "1h", label: "Past hour", hours: 1 },
  { key: "24h", label: "Past 24 hours", hours: 24 },
  { key: "7d", label: "Past 7 days", hours: 24 * 7 },
  { key: "30d", label: "Past 30 days", hours: 24 * 30 },
  { key: "all", label: "All time", hours: null },
] as const;

export type RangePreset = (typeof PRESETS)[number]["key"];

/** The start of a preset's window in microseconds, or `undefined` for all time. */
export function rangeStart(preset: RangePreset, now = Date.now()): number | undefined {
  const hours = PRESETS.find((p) => p.key === preset)?.hours;
  return hours == null ? undefined : (now - hours * 3_600_000) * 1000;
}

/** A Range filter button over the presets; there is always a range, so no Any option. */
export function DateRangeSelect({
  value,
  onChange,
}: {
  value: RangePreset;
  onChange: (v: RangePreset) => void;
}) {
  return (
    <FilterSelect
      label="Range"
      aria-label="Date range"
      value={value}
      allowAny={false}
      onChange={(v) => onChange(v as RangePreset)}
      options={PRESETS.map((p) => ({ value: p.key, label: p.label }))}
    />
  );
}
