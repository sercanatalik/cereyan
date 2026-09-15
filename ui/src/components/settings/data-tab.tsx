import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { api, unwrap } from "@/api/client";
import { KeyValueList } from "@/components/ported/key-value-list";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHead } from "@/components/ui/card";
import { Table, TableBody, TableHeader, Td, Th, Tr } from "@/components/ui/table";
import { formatTime } from "@/lib/utils";
import { RemoveProjectDialog, ResetDatabaseDialog } from "./data-dialogs";
import { bytes, count } from "./format";

/** What the store holds, and removing it. */
export function DataTab() {
  const database = useQuery({
    queryKey: ["database"],
    queryFn: async () => unwrap(await api.GET("/api/database")),
  });
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: async () => unwrap(await api.GET("/api/settings", {})),
  });
  const projects = useQuery({
    queryKey: ["projects"],
    queryFn: async () => unwrap(await api.GET("/api/projects")),
  });
  const [removing, setRemoving] = useState<string | null>(null);
  const [resetting, setResetting] = useState(false);
  const [lastReset, setLastReset] = useState<string | null>(null);
  const d = database.data;
  const s = settings.data;
  return (
    <div className="flex flex-col gap-4">
      <Card className="gap-0 py-0">
        <CardHead title="Database" />
        <CardContent className="grid grid-cols-2 gap-6 p-4">
          {d ? (
            <>
              <KeyValueList
                items={[
                  { label: "Path", value: d.path },
                  { label: "Size", value: bytes(d.bytes) },
                  { label: "WAL", value: bytes(d.wal_bytes) },
                  { label: "Retention", value: `${s?.retain_days ?? "-"} days for logs and events` },
                  {
                    label: "Secret key",
                    value: s?.secret_key_present
                      ? "present"
                      : s?.secret_key_missing
                        ? "missing (secrets unrecoverable)"
                        : "not created yet",
                  },
                ]}
              />
              <KeyValueList
                items={[
                  { label: "Runs", value: count(d.counts.runs) },
                  { label: "Task runs", value: count(d.counts.task_runs) },
                  { label: "Log lines", value: count(d.counts.logs) },
                  { label: "Events", value: count(d.counts.events) },
                  { label: "Artifacts", value: count(d.counts.artifacts) },
                ]}
              />
            </>
          ) : (
            "Loading"
          )}
        </CardContent>
      </Card>
      <Card className="gap-0 overflow-hidden py-0">
        <CardHead title="Projects" aside="Resources and variables are shared by every project" />
        <Table>
          <TableHeader>
            <tr>
              <Th className="pl-4">Project</Th>
              <Th>Flows</Th>
              <Th>Runs</Th>
              <Th>Last run</Th>
              <Th>Status</Th>
              <Th className="pr-4">
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </TableHeader>
          <TableBody>
            {(projects.data ?? []).map((p) => (
              <Tr key={p.name}>
                <Td className="pl-4 font-medium">{p.name}</Td>
                <Td>{p.flows}</Td>
                <Td>{count(p.runs)}</Td>
                <Td className="text-muted-foreground">{p.last_run_at ? formatTime(p.last_run_at) : "-"}</Td>
                <Td>
                  {p.served ? (
                    <span className="inline-flex items-center gap-1.5">
                      <span className="size-[7px] rounded-full bg-emerald-500" />
                      Served here
                    </span>
                  ) : (
                    <span className="text-muted-foreground">Not served</span>
                  )}
                </Td>
                <Td className="pr-4 text-right">
                  <Button
                    variant="ghost"
                    size="xs"
                    className="text-destructive hover:text-destructive"
                    disabled={p.served}
                    aria-label={`Remove ${p.name}`}
                    title={p.served ? "Served by this server" : undefined}
                    onClick={() => setRemoving(p.name)}
                  >
                    Remove…
                  </Button>
                </Td>
              </Tr>
            ))}
            {projects.data?.length === 0 ? (
              <Tr>
                <Td colSpan={6} className="pl-4 text-muted-foreground">
                  No projects yet.
                </Td>
              </Tr>
            ) : null}
          </TableBody>
        </Table>
        <div className="border-t px-4 py-2.5 text-xs text-muted-foreground">
          A project this server is serving can't be removed. Serve another directory first.
        </div>
      </Card>
      <Card className="gap-0 border-destructive/40 py-0">
        <div className="flex items-center border-b border-destructive/40 px-4 py-3">
          <span className="font-semibold text-destructive">Danger zone</span>
        </div>
        <div className="flex items-center justify-between gap-6 p-4">
          <div className="flex flex-col gap-0.5">
            <span className="font-semibold">Reset database</span>
            <span className="text-xs text-muted-foreground">
              Delete the run history, or everything, from db.sqlite. Runs in progress are cancelled first.
            </span>
            {lastReset ? (
              <span className="text-xs text-muted-foreground" data-testid="last-reset">
                {lastReset}
              </span>
            ) : null}
          </div>
          <Button
            variant="outline"
            className="border-destructive/40 text-destructive hover:text-destructive"
            onClick={() => setResetting(true)}
          >
            Reset database…
          </Button>
        </div>
      </Card>
      {removing ? <RemoveProjectDialog name={removing} onClose={() => setRemoving(null)} /> : null}
      {resetting ? (
        <ResetDatabaseDialog database={d} onClose={() => setResetting(false)} onDone={setLastReset} />
      ) : null}
    </div>
  );
}
