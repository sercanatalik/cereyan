import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import { api, type Run, unwrap } from "@/api/client";
import { RunForm } from "@/components/run-form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

/** A Paused run: the question it is waiting on, and a way to answer it. */
/** What a durable wait waits for, in words, or null for a question. */
function waitingFor(run: Run): string | null {
  const d = (run.state.details ?? {}) as {
    wake_at?: number;
    event?: string;
    target?: string;
    reason?: string;
  };
  const until = d.wake_at
    ? new Date(d.wake_at / 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })
    : null;
  switch (run.state.name) {
    case "Sleeping":
      return `${d.reason === "snooze" ? "Snoozed" : "Sleeping"}${until ? ` until ${until}` : ""}`;
    case "AwaitingEvent":
      return `Waiting for event ${d.event ?? ""}${until ? ` until ${until}` : ""}`;
    case "AwaitingTarget":
      return `Waiting for ${d.target ?? "a target"}${until ? `, next check ${until}` : ""}`;
    default:
      return null;
  }
}

export function PausedBanner({ run, onResumed }: { run: Run; onResumed?: () => void }) {
  const details = (run.state.details ?? {}) as { prompt?: string; schema?: any };
  const waiting = waitingFor(run);
  const schema = details.schema && typeof details.schema === "object" ? details.schema : null;
  const [text, setText] = useState("");
  const resume = useMutation({
    mutationFn: async (input: unknown) =>
      unwrap(await api.POST("/api/runs/{id}/resume", { params: { path: { id: run.id } }, body: { input } })),
    onSuccess: () => onResumed?.(),
  });
  if (waiting) {
    return (
      <div
        className="flex flex-wrap items-center gap-3 rounded-md border border-amber-400/60 bg-amber-50 p-3 text-sm dark:bg-amber-950/30"
        data-testid="paused-banner"
      >
        <span className="font-medium">{waiting}.</span>
        <span className="text-muted-foreground">The engine is free while it waits.</span>
        <Button
          size="sm"
          variant="outline"
          className="ml-auto"
          onClick={() => resume.mutate({ woke: true, early: true })}
          disabled={resume.isPending}
          data-testid="wake-run"
        >
          Wake now
        </Button>
        {resume.isError ? <span className="text-xs text-destructive">{String(resume.error)}</span> : null}
      </div>
    );
  }
  return (
    <div
      className="space-y-3 rounded-md border border-amber-400/60 bg-amber-50 p-3 dark:bg-amber-950/30"
      data-testid="paused-banner"
    >
      <div className="text-sm">
        <span className="font-medium">Waiting for input: </span>
        {details.prompt ?? run.state.message ?? "this run is paused"}
      </div>
      {schema?.properties ? (
        <RunForm
          schema={schema}
          onSubmit={(values) => resume.mutate(values)}
          busy={resume.isPending}
          submitLabel="Resume"
        />
      ) : (
        <div className="flex gap-2">
          <Input
            aria-label="Answer"
            placeholder="answer"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && resume.mutate(text)}
          />
          <Button onClick={() => resume.mutate(text)} disabled={resume.isPending} data-testid="resume-run">
            Resume
          </Button>
        </div>
      )}
      {resume.isError ? <div className="text-xs text-destructive">{String(resume.error)}</div> : null}
    </div>
  );
}
