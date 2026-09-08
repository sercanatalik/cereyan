import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { api, unwrap } from "@/api/client";
import { ArtifactsTab } from "@/components/artifacts";
import { JsonView } from "@/components/ported/json-view";
import { KeyValueList } from "@/components/ported/key-value-list";
import { RunLogs } from "@/components/ported/run-logs";
import { StateBadge } from "@/components/ported/state-badge";
import { Page } from "@/components/shell";
import { formatDuration, formatTime } from "@/lib/utils";

export const Route = createFileRoute("/task-runs/$taskRunId")({ component: TaskRunDetail });

function TaskRunDetail() {
  const { taskRunId } = Route.useParams();
  const id = Number(taskRunId);
  const query = useQuery({
    queryKey: ["task-run", id],
    queryFn: async () => unwrap(await api.GET("/api/task-runs/{id}", { params: { path: { id } } })),
  });
  const t = query.data;
  if (!t) return <Page crumbs={[{ label: "Runs", to: "/runs" }, { label: taskRunId }]}>Loading</Page>;
  return (
    <Page
      crumbs={[
        { label: "Runs", to: "/runs" },
        { label: t.run_name ?? String(t.run_id), to: `/runs/${t.run_id}` },
        { label: t.dynamic_key },
      ]}
    >
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-lg font-semibold">{t.dynamic_key}</h1>
        <StateBadge state={t.state} />
        <Link
          to="/runs/$runId"
          params={{ runId: String(t.run_id) }}
          className="text-muted-foreground hover:underline"
        >
          {t.run_name}
        </Link>
        <span className="text-muted-foreground">{formatTime(t.start_time ?? t.created_at)}</span>
        <span className="text-muted-foreground">{formatDuration(t.total_run_time)}</span>
      </div>
      {t.state.message ? (
        <div className="rounded-md border bg-card px-3 py-2 font-mono text-xs">{t.state.message}</div>
      ) : null}
      <div className="grid grid-cols-3 gap-4">
        <div className="col-span-2 space-y-4">
          <RunLogs runId={t.run_id} taskRunId={t.id} active={t.state.type === "Running"} />
          <div>
            <div className="mb-2 text-xs font-medium uppercase text-muted-foreground">Artifacts</div>
            <ArtifactsTab runId={t.run_id} taskRunId={t.id} />
          </div>
        </div>
        <KeyValueList
          items={[
            { label: "Task key", value: t.task_key },
            { label: "Flow", value: `${t.project}/${t.flow_name}` },
            { label: "Failures", value: String(t.failure_count) },
            { label: "Created", value: formatTime(t.created_at) },
            { label: "Details", value: <JsonView value={t.state.details} /> },
          ]}
        />
      </div>
    </Page>
  );
}
