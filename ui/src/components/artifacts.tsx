import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { marked } from "marked";
import { useState } from "react";
import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { PageFooter } from "@/components/pager";
import { useLiveEvent } from "@/lib/live";
import { formatTime } from "@/lib/utils";

export type Artifact = components["schemas"]["ArtifactRow"];
export type ArtifactItem = components["schemas"]["ArtifactListItem"];

/** What `/api/artifacts` filters by. */
export type ArtifactFilter = {
  kind?: string;
  key?: string;
  flow?: string;
  project?: string;
  run_id?: number;
  task_run_id?: number;
};

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

/** One artifact row: identity line, run link, and the rendered artifact. */
export function ArtifactCard({
  item,
  onHistory,
  showRun = true,
}: {
  item: ArtifactItem;
  onHistory?: (key: string) => void;
  /** Off inside a run, where every artifact is the run's own. */
  showRun?: boolean;
}) {
  return (
    <div className="rounded-md border bg-card p-3" data-testid={`artifact-item-${item.id}`}>
      <div className="mb-2 flex items-center justify-between gap-2 text-xs text-muted-foreground">
        <span className="flex items-center gap-2">
          <span className="font-medium text-foreground">{item.kind}</span>
          {item.key ? (
            onHistory ? (
              <button
                type="button"
                className="underline-offset-2 hover:underline"
                onClick={() => onHistory(item.key ?? "")}
              >
                {item.key}
              </button>
            ) : (
              <span>{item.key}</span>
            )
          ) : null}
          {showRun ? (
            <>
              <span>·</span>
              <Link
                to="/runs/$runId"
                params={{ runId: String(item.run_id) }}
                className="hover:underline"
                data-testid={`artifact-run-${item.id}`}
              >
                {item.project}/{item.flow_name} · {item.run_name}
              </Link>
            </>
          ) : null}
        </span>
        <span>{formatTime(item.updated_at)}</span>
      </div>
      <ArtifactView artifact={item} />
    </div>
  );
}

/**
 * A page of artifacts for a filter, newest first, with Previous / Next.
 * Used by the Artifacts page, the key history drawer, and a run's or task
 * run's Artifacts tab. Pages are keyset cursors (the last id of the page
 * before), so the caller's filter must stay fixed: remount on change.
 */
export function ArtifactList({
  filter,
  onHistory,
  pageSize = 20,
  showRun = true,
}: {
  filter: ArtifactFilter;
  onHistory?: (key: string) => void;
  pageSize?: number;
  showRun?: boolean;
}) {
  const [cursor, setCursor] = useState<number | undefined>(undefined);
  const [history, setHistory] = useState<number[]>([]);
  const query = useQuery({
    queryKey: ["artifacts", "list", filter, cursor, pageSize],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/artifacts", { params: { query: { ...filter, limit: pageSize, after: cursor } } }),
      ),
  });
  useLiveEvent("artifact.updated", (data) => {
    if (filter.run_id != null && data?.run_id !== filter.run_id) return;
    // Later pages are anchored by id, so only the first page moves.
    if (!cursor) query.refetch();
  });
  const items = query.data?.items ?? [];
  if (!items.length && !history.length) {
    if (query.isLoading) return null;
    return <div className="text-muted-foreground">No artifacts</div>;
  }
  return (
    <div className="space-y-3" data-testid="artifact-list">
      {items.map((a) => (
        <ArtifactCard key={a.id} item={a} onHistory={onHistory} showRun={showRun} />
      ))}
      <PageFooter
        className="flex items-center justify-between pt-1"
        from={history.length * pageSize + 1}
        count={items.length}
        noun="artifacts"
        hasPrev={history.length > 0}
        hasNext={!!query.data?.next_cursor}
        onPrev={() => {
          const prev = history.slice();
          const c = prev.pop();
          setHistory(prev);
          setCursor(c || undefined);
        }}
        onNext={() => {
          setHistory((h) => [...h, cursor ?? 0]);
          setCursor(query.data?.next_cursor ?? undefined);
        }}
      />
    </div>
  );
}

/** The Artifacts tab of a run or task run: that scope's artifacts, paged. */
export function ArtifactsTab({ runId, taskRunId }: { runId: number; taskRunId?: number }) {
  const filter: ArtifactFilter = taskRunId ? { task_run_id: taskRunId } : { run_id: runId };
  return <ArtifactList key={JSON.stringify(filter)} filter={filter} showRun={false} />;
}
