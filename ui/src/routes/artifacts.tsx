import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { ArtifactList } from "@/components/artifacts";
import { Page } from "@/components/shell";
import { Input, Select } from "@/components/ui/input";
import { Modal } from "@/components/ui/modal";
import { useProject } from "@/lib/project";

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
      <ArtifactList
        key={JSON.stringify(scoped)}
        filter={scoped}
        pageSize={50}
        onHistory={(k) => setHistory(k)}
      />
      <Modal open={history !== null} onClose={() => setHistory(null)} title={`History of ${history ?? ""}`}>
        {history !== null ? <ArtifactList key={history} filter={{ key: history }} /> : null}
      </Modal>
    </Page>
  );
}
