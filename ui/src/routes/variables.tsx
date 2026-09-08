import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { ApiError, api, unwrap } from "@/api/client";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Table, Td, Th, Tr } from "@/components/ui/table";
import { formatTime } from "@/lib/utils";

export const Route = createFileRoute("/variables")({ component: VariablesPage });

function parseValue(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

function VariablesPage() {
  const client = useQueryClient();
  const [draft, setDraft] = useState({ name: "", value: "", tags: "", secret: false });
  const [editing, setEditing] = useState<{ name: string; value: string; tags: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const vars = useQuery({
    queryKey: ["variables"],
    queryFn: async () => unwrap(await api.GET("/api/variables")),
  });
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: async () => unwrap(await api.GET("/api/settings", {})),
  });
  const invalidate = () => client.invalidateQueries({ queryKey: ["variables"] });
  const create = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/variables", {
          body: {
            name: draft.name,
            value: parseValue(draft.value) as any,
            tags: draft.tags
              .split(",")
              .map((t) => t.trim())
              .filter(Boolean),
            secret: draft.secret,
            overwrite: false,
          },
        }),
      ),
    onSuccess: () => {
      setDraft({ name: "", value: "", tags: "", secret: false });
      setError(null);
      invalidate();
    },
    onError: (e) => setError(e instanceof ApiError ? e.message : String(e)),
  });
  const update = useMutation({
    mutationFn: async (e: { name: string; value: string; tags: string }) =>
      unwrap(
        await api.PATCH("/api/variables/{name}", {
          params: { path: { name: e.name } },
          body: {
            value: parseValue(e.value) as any,
            tags: e.tags
              .split(",")
              .map((t) => t.trim())
              .filter(Boolean),
          },
        }),
      ),
    onSuccess: () => {
      setEditing(null);
      invalidate();
    },
  });
  const remove = useMutation({
    mutationFn: async (name: string) => api.DELETE("/api/variables/{name}", { params: { path: { name } } }),
    onSuccess: invalidate,
  });
  return (
    <Page crumbs={[{ label: "Variables" }]} title="Variables">
      {settings.data?.secret_key_missing ? (
        <div
          className="rounded-md border border-red-400 bg-red-50 px-3 py-2 text-sm text-red-900 dark:bg-red-900/30 dark:text-red-100"
          data-testid="secret-key-banner"
        >
          The secret key file is missing; existing secret variables are unrecoverable without it.
        </div>
      ) : null}
      <div
        className="flex flex-wrap items-end gap-2 rounded-md border bg-card p-3"
        data-testid="variable-create"
      >
        <label className="text-xs text-muted-foreground" htmlFor="var-name">
          Name
          <Input
            id="var-name"
            className="mt-1"
            value={draft.name}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            placeholder="region"
          />
        </label>
        <label className="text-xs text-muted-foreground" htmlFor="var-value">
          Value (JSON or text)
          <Input
            id="var-value"
            className="mt-1"
            value={draft.value}
            onChange={(e) => setDraft({ ...draft, value: e.target.value })}
          />
        </label>
        <label className="text-xs text-muted-foreground" htmlFor="var-tags">
          Tags
          <Input
            id="var-tags"
            className="mt-1"
            value={draft.tags}
            onChange={(e) => setDraft({ ...draft, tags: e.target.value })}
            placeholder="a, b"
          />
        </label>
        <span className="flex items-center gap-2 text-xs text-muted-foreground">
          <Checkbox
            checked={draft.secret}
            onCheckedChange={(c) => setDraft({ ...draft, secret: c === true })}
            aria-label="secret"
          />
          secret
        </span>
        <Button size="sm" onClick={() => create.mutate()} disabled={!draft.name || create.isPending}>
          Add
        </Button>
        {error ? <span className="text-xs text-red-600">{error}</span> : null}
      </div>
      <div className="rounded-md border bg-card">
        <Table>
          <thead>
            <tr>
              <Th>Name</Th>
              <Th>Value</Th>
              <Th>Tags</Th>
              <Th>Updated</Th>
              <Th />
            </tr>
          </thead>
          <tbody>
            {(vars.data ?? []).map((v) => (
              <Tr key={v.name} data-variable={v.name}>
                <Td className="font-mono text-xs">{v.name}</Td>
                <Td className="font-mono text-xs">
                  {editing?.name === v.name && !v.secret ? (
                    <Input
                      value={editing.value}
                      onChange={(e) => setEditing({ ...editing, value: e.target.value })}
                      aria-label="Edit value"
                    />
                  ) : v.secret ? (
                    <span data-testid="masked">********</span>
                  ) : (
                    JSON.stringify(v.value)
                  )}
                </Td>
                <Td>
                  {editing?.name === v.name ? (
                    <Input
                      value={editing.tags}
                      onChange={(e) => setEditing({ ...editing, tags: e.target.value })}
                      aria-label="Edit tags"
                    />
                  ) : (
                    (v.tags ?? []).join(", ")
                  )}
                </Td>
                <Td>{formatTime(v.updated_at)}</Td>
                <Td className="text-right">
                  {editing?.name === v.name ? (
                    <span className="flex justify-end gap-1">
                      <Button size="sm" onClick={() => update.mutate(editing)}>
                        Save
                      </Button>
                      <Button size="sm" variant="ghost" onClick={() => setEditing(null)}>
                        Cancel
                      </Button>
                    </span>
                  ) : (
                    <span className="flex justify-end gap-1">
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() =>
                          setEditing({
                            name: v.name,
                            value: v.secret ? "" : JSON.stringify(v.value),
                            tags: (v.tags ?? []).join(", "),
                          })
                        }
                      >
                        Edit
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => window.confirm(`Delete ${v.name}?`) && remove.mutate(v.name)}
                      >
                        Delete
                      </Button>
                    </span>
                  )}
                </Td>
              </Tr>
            ))}
            {(vars.data ?? []).length === 0 ? (
              <tr>
                <td colSpan={5} className="px-3 py-8 text-center text-muted-foreground">
                  No variables
                </td>
              </tr>
            ) : null}
          </tbody>
        </Table>
      </div>
    </Page>
  );
}
