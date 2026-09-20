import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, unwrap } from "@/api/client";
import { Button } from "@/components/ui/button";

/** A pause end or start as a short local time. */
export function pauseTime(micros: number | null | undefined): string {
  if (!micros) return "";
  return new Date(micros / 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/**
 * Shown while every schedule is paused (`POST /api/scheduler/pause`). It is
 * not dismissible: the way to make it go is to resume.
 */
export function PauseBanner() {
  const client = useQueryClient();
  const server = useQuery({
    queryKey: ["server"],
    queryFn: async () => unwrap(await api.GET("/api/server")),
    refetchInterval: (q) => (q.state.data?.paused ? 10_000 : false),
  });
  const resume = useMutation({
    mutationFn: async () => unwrap(await api.POST("/api/scheduler/resume")),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["server"] });
      client.invalidateQueries({ queryKey: ["scheduler"] });
    },
  });
  const paused = server.data?.paused;
  if (!paused) return null;
  return (
    <div
      role="alert"
      className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-amber-400/60 bg-amber-50 px-6 py-1.5 text-sm dark:bg-amber-950/30"
      data-testid="pause-banner"
    >
      <span className="font-medium">Scheduler paused{paused.reason ? `: ${paused.reason}` : ""}.</span>
      <span>
        Nothing scheduled starts{paused.until ? ` until ${pauseTime(paused.until)}` : " until resumed"}
        {paused.suppress_rules ? "; rule actions are suppressed" : ""}. Running runs, manual runs and
        backfills continue.
      </span>
      <Button
        size="sm"
        variant="outline"
        className="ml-auto"
        onClick={() => resume.mutate()}
        disabled={resume.isPending}
        data-testid="resume-scheduler"
      >
        Resume
      </Button>
      {resume.isError ? <span className="text-xs text-destructive">{String(resume.error)}</span> : null}
    </div>
  );
}
