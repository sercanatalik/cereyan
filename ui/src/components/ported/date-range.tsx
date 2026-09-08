// Ported from Prefect's DateRangeSelect (Apache 2.0); see NOTICE.
import { FilterSelect } from "@/components/filter-select";

export type RangePreset = "1h" | "24h" | "7d" | "30d" | "all";

export const RANGE_LABELS: Record<RangePreset, string> = {
  "1h": "Past hour",
  "24h": "Past 24 hours",
  "7d": "Past 7 days",
  "30d": "Past 30 days",
  all: "All time",
};

export function rangeStart(preset: RangePreset, now = Date.now()): number | undefined {
  const hours: Record<RangePreset, number | undefined> = {
    "1h": 1,
    "24h": 24,
    "7d": 168,
    "30d": 720,
    all: undefined,
  };
  const h = hours[preset];
  return h === undefined ? undefined : (now - h * 3_600_000) * 1000;
}

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
      options={(Object.keys(RANGE_LABELS) as RangePreset[]).map((k) => ({
        value: k,
        label: RANGE_LABELS[k],
      }))}
    />
  );
}
