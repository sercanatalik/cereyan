import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Modal } from "@/components/ui/modal";
import { useProject } from "@/lib/project";
import { cn } from "@/lib/utils";
import { bytes, count } from "./format";

type Database = components["schemas"]["DatabaseInfo"];
type Scope = "history" | "everything";

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function Row({ label, value, last }: { label: string; value: string; last?: boolean }) {
  return (
    <div className={cn("flex h-9 items-center justify-between gap-4 px-3", !last && "border-b")}>
      <span>{label}</span>
      <span className="font-mono text-xs">{value}</span>
    </div>
  );
}

function Confirm({
  word,
  value,
  onChange,
}: {
  word: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex flex-col gap-1 text-xs text-muted-foreground" htmlFor="confirm-word">
      <span>
        Type <code className="font-mono text-foreground">{word}</code> to confirm
      </span>
      <Input id="confirm-word" autoComplete="off" value={value} onChange={(e) => onChange(e.target.value)} />
    </label>
  );
}

/** Lists what removing a project deletes and what stays; the name must be typed. */
export function RemoveProjectDialog({ name, onClose }: { name: string; onClose: () => void }) {
  const client = useQueryClient();
  const { scope, setProject } = useProject();
  const [typed, setTyped] = useState("");
  const preview = useQuery({
    queryKey: ["project", name],
    queryFn: async () => unwrap(await api.GET("/api/projects/{name}", { params: { path: { name } } })),
  });
  const remove = useMutation({
    mutationFn: async () => unwrap(await api.DELETE("/api/projects/{name}", { params: { path: { name } } })),
    onSuccess: () => {
      if (scope === name) setProject("");
      for (const key of ["projects", "flows", "runs", "counts", "database", "events"]) {
        client.invalidateQueries({ queryKey: [key] });
      }
      onClose();
    },
  });
  const p = preview.data;
  const busy = (p?.active_runs ?? 0) > 0;
  return (
    <Modal
      open
      onClose={onClose}
      title={`Remove project ${name}`}
      description="Deletes the project's flows and everything they recorded. This can't be undone."
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            disabled={!p || busy || typed !== name || remove.isPending}
            onClick={() => remove.mutate()}
          >
            Remove project
          </Button>
        </>
      }
    >
      <div className="flex flex-col rounded-lg border" data-testid="removal-preview">
        <Row label="Flows" value={count(p?.flows)} />
        <Row label="Runs, with their task runs, logs and artifacts" value={count(p?.runs)} />
        <Row label="Schedules and backfills" value={`${count(p?.schedules)} · ${count(p?.backfills)}`} />
        <Row label="Events about these flows" value={count(p?.events)} last />
      </div>
      <p className="text-xs text-muted-foreground">
        Kept:{" "}
        {p?.matching_rules ? (
          <>
            {p.matching_rules} {p.matching_rules === 1 ? "rule" : "rules"} that match project{" "}
            <code className="font-mono">{name}</code> (they won't fire until it is served again), and{" "}
          </>
        ) : null}
        the variables and resources every project shares.
      </p>
      {busy ? (
        <p className="text-xs text-destructive">
          {p?.active_runs} {p?.active_runs === 1 ? "run" : "runs"} of this project are in progress. Cancel
          them or let them finish first.
        </p>
      ) : null}
      <Confirm word={name} value={typed} onChange={setTyped} />
      {remove.isError ? <p className="text-xs text-destructive">{errorText(remove.error)}</p> : null}
    </Modal>
  );
}

/** Two scopes, an optional copy first, and `reset` typed to confirm. */
export function ResetDatabaseDialog({
  database,
  onClose,
  onDone,
}: {
  database: Database | undefined;
  onClose: () => void;
  onDone: (message: string) => void;
}) {
  const client = useQueryClient();
  const [scope, setScope] = useState<Scope>("history");
  const [backup, setBackup] = useState(true);
  const [typed, setTyped] = useState("");
  const reset = useMutation({
    mutationFn: async () => unwrap(await api.POST("/api/database/reset", { body: { scope, backup } })),
    onSuccess: (result) => {
      // Every open page learns of the reset from the stream too; this one reloads now.
      client.invalidateQueries();
      onDone(result.backup_path ? `Reset done. The copy is at ${result.backup_path}.` : "Reset done.");
      onClose();
    },
  });
  const c = database?.counts;
  const options: { value: Scope; title: string; text: string; counts: string }[] = [
    {
      value: "history",
      title: "Run history",
      text: "Runs, task runs, logs, events, artifacts, backfills and rule firings. Flows, schedules, rules, variables and settings stay.",
      counts: `${count(c?.runs)} runs · ${count(c?.logs)} log lines · ${count(c?.events)} events`,
    },
    {
      value: "everything",
      title: "Everything",
      text: "Also projects that aren't served here, rules and schedules made in the UI, and variables including secrets. What this server registered from code stays, and so do settings.",
      counts: `${count(database?.stale_projects)} projects · ${count(c?.ui_rules)} rules · ${count(c?.variables)} variables`,
    },
  ];
  return (
    <Modal
      open
      onClose={onClose}
      title="Reset database"
      description="Runs in progress are cancelled first. This can't be undone."
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            disabled={typed !== "reset" || reset.isPending}
            onClick={() => reset.mutate()}
          >
            {scope === "history" ? "Delete run history" : "Reset everything"}
          </Button>
        </>
      }
    >
      <div role="radiogroup" aria-label="What to delete" className="flex flex-col gap-2">
        {options.map((o) => {
          const on = scope === o.value;
          return (
            <label
              key={o.value}
              className={cn(
                "flex cursor-pointer gap-3 rounded-lg border p-3 has-[:focus-visible]:ring-[3px] has-[:focus-visible]:ring-ring/50",
                on ? "border-primary bg-muted/50" : "hover:bg-muted/30",
              )}
            >
              <input
                type="radio"
                name="reset-scope"
                value={o.value}
                checked={on}
                onChange={() => setScope(o.value)}
                className="sr-only"
              />
              <span
                aria-hidden="true"
                className={cn(
                  "mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-full border",
                  on && "border-primary",
                )}
              >
                {on ? <span className="size-2 rounded-full bg-primary" /> : null}
              </span>
              <span className="flex flex-col gap-0.5">
                <span className="font-semibold">{o.title}</span>
                <span className="text-xs text-muted-foreground">{o.text}</span>
                <span className="mt-1 font-mono text-xs">{o.counts}</span>
              </span>
            </label>
          );
        })}
      </div>
      <div className="flex gap-2.5">
        <Checkbox
          id="reset-backup"
          className="mt-0.5"
          checked={backup}
          onCheckedChange={(checked) => setBackup(checked === true)}
        />
        <label htmlFor="reset-backup" className="flex min-w-0 flex-col gap-0.5">
          <span className="font-medium">Save a copy first</span>
          {database ? (
            <span className="break-all font-mono text-xs text-muted-foreground">
              {database.backup_dir}/db-&lt;time&gt;.sqlite · about {bytes(database.bytes)}
            </span>
          ) : null}
        </label>
      </div>
      <Confirm word="reset" value={typed} onChange={setTyped} />
      {reset.isError ? <p className="text-xs text-destructive">{errorText(reset.error)}</p> : null}
    </Modal>
  );
}
