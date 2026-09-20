import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api, unwrap } from "@/api/client";
import { pauseTime } from "@/components/pause-banner";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHead } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";

/** What you edit (title, resources, defaults) and what is running (custom routes, engines). */
export function GeneralTab() {
  const client = useQueryClient();
  const info = useQuery({
    queryKey: ["server"],
    queryFn: async () => unwrap(await api.GET("/api/server")),
    refetchInterval: 5000,
  });
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: async () => unwrap(await api.GET("/api/settings", {})),
  });
  const [resources, setResources] = useState<{ name: string; total: string }[]>([]);
  const [retain, setRetain] = useState("");
  const [retainRuns, setRetainRuns] = useState("");
  const [retainFailed, setRetainFailed] = useState("");
  const [keepLast, setKeepLast] = useState("");
  const [backupEvery, setBackupEvery] = useState("");
  const [backupKeep, setBackupKeep] = useState("");
  const [crash, setCrash] = useState("");
  const [title, setTitle] = useState("");
  const [pauseReason, setPauseReason] = useState("");
  const [pauseUntil, setPauseUntil] = useState("");
  const [suppressRules, setSuppressRules] = useState(false);
  const pauseScheduler = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.POST("/api/scheduler/pause", {
          body: {
            reason: pauseReason.trim() || null,
            until: pauseUntil ? new Date(pauseUntil).getTime() * 1000 : null,
            suppress_rules: suppressRules,
          },
        }),
      ),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["server"] });
      client.invalidateQueries({ queryKey: ["scheduler"] });
    },
  });
  const resumeScheduler = useMutation({
    mutationFn: async () => unwrap(await api.POST("/api/scheduler/resume")),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["server"] });
      client.invalidateQueries({ queryKey: ["scheduler"] });
    },
  });
  useEffect(() => {
    const s = settings.data;
    if (!s) return;
    setTitle(s.title === "cereyan" ? "" : s.title);
    setResources(
      Object.entries(s.resources as Record<string, { total: number }>).map(([name, v]) => ({
        name,
        total: String(v.total),
      })),
    );
    setRetain(String(s.retain_days));
    setRetainRuns(String(s.retain_runs_days));
    setRetainFailed(String(s.retain_failed_runs_days));
    setKeepLast(String(s.keep_last_runs_per_flow));
    setBackupEvery(String(s.backup_every));
    setBackupKeep(String(s.backup_keep));
    setCrash(String(s.crash_retries_default));
  }, [settings.data]);
  const save = useMutation({
    mutationFn: async () =>
      unwrap(
        await api.PATCH("/api/settings", {
          body: {
            resources: Object.fromEntries(
              resources.filter((r) => r.name).map((r) => [r.name, Number(r.total) || 0]),
            ),
            retain_days: Number(retain),
            retain_runs_days: Number(retainRuns),
            retain_failed_runs_days: Number(retainFailed),
            keep_last_runs_per_flow: Number(keepLast),
            backup_every: Number(backupEvery),
            backup_keep: Number(backupKeep),
            crash_retries: Number(crash),
          },
        }),
      ),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["settings"] });
      client.invalidateQueries({ queryKey: ["environment"] });
    },
  });
  // Sends only the title, so saving it does not resend the other cards' fields.
  const saveTitle = useMutation({
    mutationFn: async () => unwrap(await api.PATCH("/api/settings", { body: { title } })),
    onSuccess: (saved) => {
      client.setQueryData(["server"], (old: typeof info.data) =>
        old ? { ...old, title: saved.title } : old,
      );
      document.title = saved.title;
      client.invalidateQueries({ queryKey: ["server"] });
      client.invalidateQueries({ queryKey: ["settings"] });
      client.invalidateQueries({ queryKey: ["environment"] });
    },
  });
  const s = settings.data;
  const srv = info.data;
  return (
    <div className="grid grid-cols-2 gap-4">
      <Card className="col-span-2 gap-0 py-0">
        <CardHead title="Interface" />
        <CardContent className="p-4">
          <div className="flex flex-wrap items-end gap-x-4 gap-y-2">
            <div className="flex items-end gap-2">
              <label className="block text-xs text-muted-foreground" htmlFor="ui-title">
                Title
                <Input
                  id="ui-title"
                  className="mt-1 w-80"
                  value={title}
                  placeholder="cereyan"
                  maxLength={80}
                  onChange={(e) => setTitle(e.target.value)}
                />
              </label>
              <Button
                size="sm"
                className="mb-0.5"
                onClick={() => saveTitle.mutate()}
                disabled={saveTitle.isPending}
              >
                Save
              </Button>
            </div>
            <p className="min-w-72 flex-1 pb-1.5 text-xs text-muted-foreground">
              Shown next to the mark in the top bar and as the browser tab title. Leave it empty to show
              cereyan. Saved to cereyan.toml as <code className="font-mono">[ui] title</code>.
            </p>
          </div>
          {saveTitle.isError ? (
            <p className="mt-2 text-xs text-destructive">
              The title was not saved: use at most 80 characters and no control characters.
            </p>
          ) : null}
        </CardContent>
      </Card>
      <Card className="gap-0 py-0">
        <CardHead title="Resources" />
        <CardContent className="space-y-2 p-4" data-testid="resources-editor">
          {resources
            .map((r, i) => ({ r, i, key: `${i}:${r.name}` }))
            .map(({ r, i, key }) => (
              <div key={key} className="flex items-center gap-2">
                <Input
                  value={r.name}
                  aria-label="Resource name"
                  onChange={(e) =>
                    setResources(resources.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))
                  }
                />
                <Input
                  value={r.total}
                  aria-label={`Total for ${r.name}`}
                  className="w-24"
                  onChange={(e) =>
                    setResources(resources.map((x, j) => (j === i ? { ...x, total: e.target.value } : x)))
                  }
                />
              </div>
            ))}
          <Button
            size="sm"
            variant="outline"
            onClick={() => setResources([...resources, { name: "", total: "1" }])}
          >
            Add resource
          </Button>
        </CardContent>
      </Card>
      <Card className="gap-0 py-0">
        <CardHead title="Defaults" />
        <CardContent className="space-y-2 p-4">
          <label className="block text-xs text-muted-foreground" htmlFor="retain">
            Retention days (logs and events)
            <Input
              id="retain"
              className="mt-1 w-32"
              value={retain}
              onChange={(e) => setRetain(e.target.value)}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="retain-runs">
            Retention days (runs; 0 keeps them)
            <Input
              id="retain-runs"
              className="mt-1 w-32"
              value={retainRuns}
              onChange={(e) => setRetainRuns(e.target.value)}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="retain-failed">
            Retention days (failed and crashed runs; 0 uses the run value)
            <Input
              id="retain-failed"
              className="mt-1 w-32"
              value={retainFailed}
              onChange={(e) => setRetainFailed(e.target.value)}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="keep-last">
            Runs kept per flow
            <Input
              id="keep-last"
              className="mt-1 w-32"
              value={keepLast}
              onChange={(e) => setKeepLast(e.target.value)}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="backup-every">
            Backup every (hours; 0 is off)
            <Input
              id="backup-every"
              className="mt-1 w-32"
              value={backupEvery}
              onChange={(e) => setBackupEvery(e.target.value)}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="backup-keep">
            Backups kept
            <Input
              id="backup-keep"
              className="mt-1 w-32"
              value={backupKeep}
              onChange={(e) => setBackupKeep(e.target.value)}
            />
          </label>
          <label className="block text-xs text-muted-foreground" htmlFor="crash">
            Crash retries
            <Input
              id="crash"
              className="mt-1 w-32"
              value={crash}
              onChange={(e) => setCrash(e.target.value)}
            />
          </label>
          <div className="text-xs text-muted-foreground">
            Catch-up {s?.catchup_default} · max engines {s?.max_engines} · engine max runs{" "}
            {s?.engine_max_runs}
          </div>
          <Button size="sm" onClick={() => save.mutate()} disabled={save.isPending}>
            Save settings
          </Button>
          {save.isSuccess ? (
            <span className="ml-2 text-xs text-muted-foreground">Saved to cereyan.toml</span>
          ) : null}
        </CardContent>
      </Card>
      <Card className="col-span-2 gap-0 py-0">
        <CardHead title="Custom routes" />
        <CardContent className="p-4">
          {s?.custom_routes.length ? (
            <table className="w-full text-sm">
              <tbody>
                {s.custom_routes.map((r) => (
                  <tr key={`${r.method}-${r.path}`} className="border-t">
                    <td className="py-1 font-mono text-xs">{r.method}</td>
                    <td className="font-mono text-xs">{r.path}</td>
                    <td className="text-xs text-muted-foreground">{r.source ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : (
            <span className="text-muted-foreground">No custom routes registered.</span>
          )}
        </CardContent>
      </Card>
      <Card className="col-span-2 gap-0 py-0" data-testid="scheduler-card">
        <CardHead title="Scheduler" />
        <CardContent className="p-4">
          {srv?.paused ? (
            <div className="flex flex-wrap items-center gap-3 text-sm">
              <span>
                <span className="font-medium">Paused since {pauseTime(srv.paused.since)}</span>
                {srv.paused.reason ? `: ${srv.paused.reason}` : ""}
                {srv.paused.until ? `, until ${pauseTime(srv.paused.until)}` : ""}
                {srv.paused.suppress_rules ? "; rule actions suppressed" : ""}.
              </span>
              <Button
                size="sm"
                onClick={() => resumeScheduler.mutate()}
                disabled={resumeScheduler.isPending}
                data-testid="resume-scheduler-card"
              >
                Resume
              </Button>
            </div>
          ) : (
            <div className="flex flex-wrap items-end gap-x-4 gap-y-2">
              <label className="block text-xs text-muted-foreground" htmlFor="pause-reason">
                Reason
                <Input
                  id="pause-reason"
                  className="mt-1 w-64"
                  value={pauseReason}
                  placeholder="db upgrade"
                  onChange={(e) => setPauseReason(e.target.value)}
                />
              </label>
              <label className="block text-xs text-muted-foreground" htmlFor="pause-until">
                Resume at (optional)
                <Input
                  id="pause-until"
                  type="datetime-local"
                  className="mt-1"
                  value={pauseUntil}
                  onChange={(e) => setPauseUntil(e.target.value)}
                />
              </label>
              <label
                className="flex items-center gap-2 pb-2 text-xs text-muted-foreground"
                htmlFor="pause-suppress"
              >
                <Checkbox
                  id="pause-suppress"
                  checked={suppressRules}
                  onCheckedChange={(c) => setSuppressRules(c === true)}
                  aria-label="suppress rule actions"
                />
                suppress rule actions
              </label>
              <Button
                size="sm"
                className="mb-0.5"
                onClick={() => pauseScheduler.mutate()}
                disabled={pauseScheduler.isPending}
                data-testid="pause-scheduler"
              >
                Pause every schedule
              </Button>
              <p className="min-w-72 flex-1 pb-1.5 text-xs text-muted-foreground">
                Holds every schedule at once; running runs, manual runs and backfills continue. Held runs
                start on resume and each schedule catches up under its own policy.
              </p>
            </div>
          )}
          {pauseScheduler.isError ? (
            <p className="mt-2 text-xs text-destructive">{String(pauseScheduler.error)}</p>
          ) : null}
        </CardContent>
      </Card>
      <Card className="col-span-2 gap-0 py-0">
        <CardHead title="Engines" />
        <CardContent className="p-4">
          {srv?.engines.length ? (
            <table className="w-full text-sm">
              <thead>
                <tr className="text-left text-xs uppercase text-muted-foreground">
                  <th className="py-1">PID</th>
                  <th>Module</th>
                  <th>Runs done</th>
                  <th>Current run</th>
                </tr>
              </thead>
              <tbody>
                {srv.engines.map((e: any) => (
                  <tr key={e.id} className="border-t">
                    <td className="py-1 font-mono text-xs">{e.pid}</td>
                    <td>{e.module}</td>
                    <td>{e.runs_done}</td>
                    <td>{e.current_run ?? "-"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : (
            <span className="text-muted-foreground">No engines running.</span>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
