import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { api, unwrap } from "@/api/client";
import { KeyValueList } from "@/components/ported/key-value-list";
import { Page } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHead } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { formatTime } from "@/lib/utils";

export const Route = createFileRoute("/settings")({ component: SettingsPage });

function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function SettingsPage() {
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
  const [crash, setCrash] = useState("");
  useEffect(() => {
    const s = settings.data;
    if (!s) return;
    setResources(
      Object.entries(s.resources as Record<string, { total: number }>).map(([name, v]) => ({
        name,
        total: String(v.total),
      })),
    );
    setRetain(String(s.retain_days));
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
            crash_retries: Number(crash),
          },
        }),
      ),
    onSuccess: () => client.invalidateQueries({ queryKey: ["settings"] }),
  });
  const s = settings.data;
  const srv = info.data;
  return (
    <Page crumbs={[{ label: "Settings" }]} title="Settings">
      <div className="grid grid-cols-2 gap-4">
        <Card className="gap-0 py-0">
          <CardHead title="Server" />
          <CardContent className="p-4">
            {s ? (
              <KeyValueList
                items={[
                  { label: "Version", value: s.version },
                  { label: "URL", value: `http://${s.host}:${s.port}` },
                  { label: "PID", value: String(s.pid) },
                  { label: "Home", value: s.home },
                  { label: "Served directory", value: s.served_dir ?? "-" },
                  { label: "Started", value: srv ? formatTime(srv.started_at) : "-" },
                  { label: "Email", value: s.email_configured ? "configured" : "not configured" },
                  {
                    label: "Secret key",
                    value: s.secret_key_present
                      ? "present"
                      : s.secret_key_missing
                        ? "missing (secrets unrecoverable)"
                        : "not created yet",
                  },
                ]}
              />
            ) : (
              "Loading"
            )}
          </CardContent>
        </Card>
        <Card className="gap-0 py-0">
          <CardHead title="Database" />
          <CardContent className="p-4">
            {s ? (
              <KeyValueList
                items={[
                  { label: "Path", value: s.database_path },
                  { label: "Size", value: bytes(s.database_bytes) },
                  { label: "WAL", value: bytes(s.wal_bytes) },
                  { label: "Retention", value: `${s.retain_days} days for logs and events` },
                ]}
              />
            ) : (
              "Loading"
            )}
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
    </Page>
  );
}
