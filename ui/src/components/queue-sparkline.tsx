import { useQuery } from "@tanstack/react-query";
import { api, unwrap } from "@/api/client";

/** The last hour of queue depth from `/api/metrics/history`, refreshed every five seconds. */
export function QueueSparkline({ width = 160, height = 32 }: { width?: number; height?: number }) {
  const history = useQuery({
    queryKey: ["metrics-history"],
    queryFn: async () => unwrap(await api.GET("/api/metrics/history")),
    refetchInterval: 5000,
  });
  const samples = history.data?.samples ?? [];
  const values = samples.map((s) => s.queued);
  const max = Math.max(1, ...values);
  const step = values.length > 1 ? width / (values.length - 1) : width;
  const points = values.map(
    (v, i) => `${(i * step).toFixed(1)},${(height - 2 - (v / max) * (height - 4)).toFixed(1)}`,
  );
  const latest = values.at(-1) ?? 0;
  return (
    <div
      className="flex flex-col items-end gap-0.5"
      data-testid="queue-sparkline"
      title="Queue depth, last hour"
    >
      <span className="text-[26px] font-semibold leading-8 tracking-tight tabular-nums">{latest}</span>
      <span className="text-xs text-muted-foreground">queued</span>
      <svg
        width={width}
        height={height}
        viewBox={`0 0 ${width} ${height}`}
        aria-label={`queue depth over ${values.length} samples`}
      >
        <polyline
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          className="text-muted-foreground"
          points={points.length > 1 ? points.join(" ") : `0,${height - 2} ${width},${height - 2}`}
        />
      </svg>
    </div>
  );
}
