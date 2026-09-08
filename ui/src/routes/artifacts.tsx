import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useState } from "react";
import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { ArtifactView } from "@/components/artifacts";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/input";
import { Modal } from "@/components/ui/modal";
import { useLiveEvent } from "@/lib/live";
import { useProject } from "@/lib/project";
import { formatTime } from "@/lib/utils";

export type ArtifactItem = components["schemas"]["ArtifactListItem"];
type Search = { kind?: string; key?: string; flow?: string; project?: string };

export const Route = createFileRoute("/artifacts")({
  validateSearch: (s: Record<string, unknown>): Search => ({
    kind: typeof s.kind === "string" ? s.kind : undefined,
    key: typeof s.key === "string" ? s.key : undefined,
    flow: typeof s.flow === "string" ? s.flow : undefined,
    project: typeof s.project === "string" ? s.project : undefined,
  }),
  component: ArtifactsPage,
});

const KINDS = ["markdown", "table", "progress", "link", "image"];

/** One artifact row: identity line, run link, and the rendered artifact. */
export function ArtifactCard({ item, onHistory }: { item: ArtifactItem; onHistory?: (key: string) => void }) {
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
          <span>·</span>
          <Link
            to="/runs/$runId"
            params={{ runId: String(item.run_id) }}
            className="hover:underline"
            data-testid={`artifact-run-${item.id}`}
          >
            {item.project}/{item.flow_name} · {item.run_name}
          </Link>
        </span>
        <span>{formatTime(item.updated_at)}</span>
      </div>
      <ArtifactView artifact={item} />
    </div>
  );
}

/** The list for a filter; also used by the key history drawer. */
export function ArtifactList({
  filter,
  onHistory,
  pageSize = 50,
}: {
  filter: Search & { run_id?: number };
  onHistory?: (key: string) => void;
  pageSize?: number;
}) {
  const [cursor, setCursor] = useState<number | undefined>(undefined);
  const [pages, setPages] = useState<ArtifactItem[][]>([]);
  const query = useQuery({
    queryKey: ["artifacts", "list", filter, cursor, pageSize],
    queryFn: async () =>
      unwrap(
        await api.GET("/api/artifacts", { params: { query: { ...filter, limit: pageSize, after: cursor } } }),
      ),
  });
  useLiveEvent("artifact.updated", () => {
    if (!cursor) query.refetch();
  });
  const items = [...pages.flat(), ...(query.data?.items ?? [])];
  if (!items.length && !query.isLoading) return <div className="text-muted-foreground">No artifacts</div>;
  return (
    <div className="space-y-3">
      {items.map((a) => (
        <ArtifactCard key={a.id} item={a} onHistory={onHistory} />
      ))}
      {query.data?.next_cursor ? (
        <Button
          variant="outline"
          onClick={() => {
            setPages((p) => [...p, query.data?.items ?? []]);
            setCursor(query.data?.next_cursor ?? undefined);
          }}
        >
          Load more
        </Button>
      ) : null}
    </div>
  );
}

function ArtifactsPage() {
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const [history, setHistory] = useState<string | null>(null);
  const { project } = useProject(search.project);
  const scoped = { ...search, project };
  const set = (patch: Partial<Search>) =>
    navigate({ search: (prev: Search) => ({ ...prev, ...patch }) as Search, replace: true });
  return (
    <Page crumbs={[{ label: "Artifacts" }]} title="Artifacts">
      <div className="mb-4 flex flex-wrap items-center gap-2">
        <Select
          value={search.kind ?? ""}
          onChange={(e) => set({ kind: e.target.value || undefined })}
          aria-label="Kind"
        >
          <option value="">Any kind</option>
          {KINDS.map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </Select>
        <Input
          placeholder="key"
          aria-label="Key"
          value={search.key ?? ""}
          onChange={(e) => set({ key: e.target.value || undefined })}
          className="w-40"
        />
        <Input
          placeholder="flow"
          aria-label="Flow"
          value={search.flow ?? ""}
          onChange={(e) => set({ flow: e.target.value || undefined })}
          className="w-40"
        />
        <Input
          placeholder="project"
          aria-label="Project"
          value={search.project ?? ""}
          onChange={(e) => set({ project: e.target.value || undefined })}
          className="w-40"
        />
      </div>
      <ArtifactList key={JSON.stringify(scoped)} filter={scoped} onHistory={(k) => setHistory(k)} />
      <Modal open={history !== null} onClose={() => setHistory(null)} title={`History of ${history ?? ""}`}>
        {history !== null ? <ArtifactList key={history} filter={{ key: history }} pageSize={20} /> : null}
      </Modal>
    </Page>
  );
}
