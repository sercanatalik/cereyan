import { useMemo, useState } from "react";
import type { Flow } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/input";
import { Modal } from "@/components/ui/modal";

export function dateParameters(flow: Pick<Flow, "parameter_schema">): string[] {
  const props: Record<string, any> = (flow.parameter_schema as any)?.properties ?? {};
  return Object.entries(props)
    .filter(([, p]) => {
      const fmt = p?.format ?? p?.anyOf?.find((x: any) => x?.format)?.format;
      return fmt === "date" || fmt === "date-time";
    })
    .map(([name]) => name);
}

export function intervalSeconds(text: string): number | null {
  const m = /^\s*(\d+(?:\.\d+)?)\s*([smhdw]?)\s*$/i.exec(text);
  if (!m) return null;
  const n = Number(m[1]);
  const mult: Record<string, number> = { "": 1, s: 1, m: 60, h: 3600, d: 86400, w: 604800 };
  return n > 0 ? n * mult[m[2].toLowerCase()] : null;
}

export function countRuns(start: string, end: string, interval: string): number | null {
  const secs = intervalSeconds(interval);
  const s = Date.parse(start);
  const e = Date.parse(end);
  if (!secs || Number.isNaN(s) || Number.isNaN(e) || e < s) return null;
  return Math.floor((e - s) / 1000 / secs) + 1;
}

export function BackfillDialog({
  flow,
  open,
  onClose,
  onSubmit,
  busy,
  error,
}: {
  flow: Flow;
  open: boolean;
  onClose: () => void;
  onSubmit: (body: {
    parameter: string;
    start: string;
    end: string;
    interval: string;
    concurrency: number;
  }) => void;
  busy?: boolean;
  error?: string | null;
}) {
  const params = useMemo(() => dateParameters(flow), [flow]);
  const [parameter, setParameter] = useState(params[0] ?? "");
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  const [interval, setInterval] = useState("1d");
  const [concurrency, setConcurrency] = useState(1);
  const count = countRuns(start, end, interval);
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={`Backfill ${flow.name}`}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button
            disabled={!count || !parameter || busy}
            onClick={() => onSubmit({ parameter, start, end, interval, concurrency })}
          >
            Create {count ?? ""} runs
          </Button>
        </>
      }
    >
      {params.length === 0 ? (
        <div className="text-sm text-muted-foreground">
          This flow has no date or datetime parameter to backfill over.
        </div>
      ) : (
        <div className="grid grid-cols-2 gap-3" data-testid="backfill-dialog">
          <label className="text-xs text-muted-foreground" htmlFor="bf-param">
            Parameter
            <Select
              id="bf-param"
              value={parameter}
              onChange={(e) => setParameter(e.target.value)}
              className="mt-1 w-full"
            >
              {params.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </Select>
          </label>
          <label className="text-xs text-muted-foreground" htmlFor="bf-conc">
            Concurrency
            <Input
              type="number"
              min={1}
              className="mt-1"
              value={concurrency}
              onChange={(e) => setConcurrency(Math.max(1, Number(e.target.value)))}
            />
          </label>
          <label className="text-xs text-muted-foreground" htmlFor="bf-start">
            Start
            <Input
              type="date"
              className="mt-1"
              value={start}
              onChange={(e) => setStart(e.target.value)}
              aria-label="Start"
            />
          </label>
          <label className="text-xs text-muted-foreground" htmlFor="bf-end">
            End
            <Input
              type="date"
              className="mt-1"
              value={end}
              onChange={(e) => setEnd(e.target.value)}
              aria-label="End"
            />
          </label>
          <label className="text-xs text-muted-foreground" htmlFor="bf-interval">
            Interval
            <Input
              className="mt-1"
              value={interval}
              onChange={(e) => setInterval(e.target.value)}
              placeholder="1d, 12h, 3600"
              aria-label="Interval"
            />
          </label>
          <div className="self-end text-sm" data-testid="backfill-count">
            {count === null ? "Pick a range and interval" : `${count} runs will be created`}
          </div>
          {error ? <div className="col-span-2 text-xs text-red-600">{error}</div> : null}
        </div>
      )}
    </Modal>
  );
}
