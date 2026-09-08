import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import { api, type Run, unwrap } from "@/api/client";
import { RunForm } from "@/components/run-form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

/** A Paused run: the question it is waiting on, and a way to answer it. */
export function PausedBanner({ run, onResumed }: { run: Run; onResumed?: () => void }) {
  const details = (run.state.details ?? {}) as { prompt?: string; schema?: any };
  const schema = details.schema && typeof details.schema === "object" ? details.schema : null;
  const [text, setText] = useState("");
  const resume = useMutation({
    mutationFn: async (input: unknown) =>
      unwrap(await api.POST("/api/runs/{id}/resume", { params: { path: { id: run.id } }, body: { input } })),
    onSuccess: () => onResumed?.(),
  });
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
