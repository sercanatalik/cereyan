import { useQuery } from "@tanstack/react-query";
import { marked } from "marked";
import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { useLiveEvent } from "@/lib/live";
import { formatTime } from "@/lib/utils";

export type Artifact = components["schemas"]["ArtifactRow"];

export function ArtifactView({ artifact }: { artifact: Artifact }) {
  const d = artifact.data as any;
  switch (artifact.kind) {
    case "markdown":
      return (
        <div
          className="prose prose-sm max-w-none"
          // biome-ignore lint/security/noDangerouslySetInnerHtml: markdown authored by the user's own flow code
          dangerouslySetInnerHTML={{ __html: marked.parse(String(d?.text ?? "")) as string }}
        />
      );
    case "table": {
      const columns: string[] = d?.columns ?? [];
      const rows: any[] = d?.rows ?? [];
      const keyed = rows.map((r, i) => ({ r, key: `${i}:${JSON.stringify(r).slice(0, 60)}` }));
      return (
        <div className="overflow-x-auto">
          <table className="w-full text-sm" data-testid="artifact-table">
            <thead>
              <tr>
                {columns.map((c) => (
                  <th key={c} className="px-2 py-1 text-left text-xs uppercase text-muted-foreground">
                    {c}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {keyed.map(({ r, key }) => (
                <tr key={key} className="border-t">
                  {(Array.isArray(r) ? r : columns.map((c) => r?.[c])).map((cell: unknown, j: number) => (
                    <td key={`${key}-${columns[j] ?? String(j)}`} className="px-2 py-1 font-mono text-xs">
                      {typeof cell === "object" ? JSON.stringify(cell) : String(cell ?? "")}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    }
    case "progress": {
      const value = Math.max(0, Math.min(100, Number(d?.value ?? 0)));
      return (
        <div className="space-y-1" data-testid="artifact-progress">
          <div className="flex justify-between text-xs text-muted-foreground">
            <span>{d?.label ?? artifact.key ?? "progress"}</span>
            <span>{value}%</span>
          </div>
          <div className="h-2 w-full rounded bg-accent">
            <div className="h-2 rounded bg-primary transition-all" style={{ width: `${value}%` }} />
          </div>
        </div>
      );
    }
    case "link":
      return (
        <a href={String(d?.url ?? "#")} className="text-primary underline" target="_blank" rel="noreferrer">
          {String(d?.text ?? d?.url ?? "link")}
        </a>
      );
    case "image":
      return (
        <img
          src={String(d?.src ?? "")}
          alt={artifact.key ?? "artifact"}
          className="max-h-96 rounded border"
        />
      );
    default:
      return <pre className="text-xs">{JSON.stringify(d, null, 2)}</pre>;
  }
}

export function ArtifactsTab({ runId, taskRunId }: { runId: number; taskRunId?: number }) {
  const key = taskRunId ? ["artifacts", "task", taskRunId] : ["artifacts", "run", runId];
  const query = useQuery({
    queryKey: key,
    queryFn: async () =>
      taskRunId
        ? unwrap(await api.GET("/api/task-runs/{id}/artifacts", { params: { path: { id: taskRunId } } }))
        : unwrap(await api.GET("/api/runs/{id}/artifacts", { params: { path: { id: runId } } })),
  });
  useLiveEvent("artifact.updated", (data) => {
    if (data?.run_id === runId) query.refetch();
  });
  const items = query.data ?? [];
  if (!items.length) return <div className="text-muted-foreground">No artifacts</div>;
  return (
    <div className="space-y-3">
      {items.map((a) => (
        <div key={a.id} className="rounded-md border bg-card p-3" data-testid={`artifact-${a.kind}`}>
          <div className="mb-2 flex items-center justify-between text-xs text-muted-foreground">
            <span>
              {a.kind}
              {a.key ? ` · ${a.key}` : ""}
            </span>
            <span>{formatTime(a.updated_at)}</span>
          </div>
          <ArtifactView artifact={a} />
        </div>
      ))}
    </div>
  );
}
