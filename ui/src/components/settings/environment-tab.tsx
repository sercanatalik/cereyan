import { useQuery } from "@tanstack/react-query";
import { Fragment, useState } from "react";
import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";
import { KeyValueList } from "@/components/ported/key-value-list";
import { Card, CardContent, CardHead } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Segmented } from "@/components/ui/segmented";
import { Switch } from "@/components/ui/switch";
import { cn, formatTime } from "@/lib/utils";

type Entry = components["schemas"]["ConfigEntry"];
type Variable = components["schemas"]["EnvVariable"];

/** Tables in the order cereyan.toml documents them. */
const TABLES = ["server", "defaults", "ui", "resources", "email"];
const MASK = "••••••••";
/** Process variables listed before "Show more". */
const PROCESS_PREVIEW = 12;

function Chip({ children, mono = true }: { children: React.ReactNode; mono?: boolean }) {
  return (
    <span
      className={cn(
        "inline-flex h-5 items-center whitespace-nowrap rounded-sm bg-muted px-1.5 text-[11px] leading-4",
        mono && "font-mono",
      )}
    >
      {children}
    </span>
  );
}

function Source({ entry }: { entry: Entry }) {
  if (entry.source === "default") return <span className="text-xs text-muted-foreground">default</span>;
  if (entry.source === "settings") return <Chip mono={false}>edited in Settings</Chip>;
  return <Chip>{entry.source_name ?? entry.source}</Chip>;
}

function value(entry: Entry): React.ReactNode {
  if (entry.secret) return <span className="text-muted-foreground">{MASK}</span>;
  const v = entry.value as unknown;
  if (v === null || v === undefined) return <span className="font-sans text-muted-foreground">not set</span>;
  if (entry.table === "server" && entry.key === "base_path" && v === "") return "/";
  return typeof v === "string" ? v : JSON.stringify(v);
}

/** What is in effect and where each value came from. */
export function EnvironmentTab() {
  const env = useQuery({
    queryKey: ["environment"],
    queryFn: async () => unwrap(await api.GET("/api/settings/environment")),
  });
  const settings = useQuery({
    queryKey: ["settings"],
    queryFn: async () => unwrap(await api.GET("/api/settings", {})),
  });
  const info = useQuery({ queryKey: ["server"], queryFn: async () => unwrap(await api.GET("/api/server")) });
  const e = env.data;
  const s = settings.data;
  const srv = info.data;
  const homeFromEnv = e?.variables.some((v) => v.name === "CEREYAN_HOME");
  return (
    <div className="flex flex-col gap-4">
      <Card className="gap-0 py-0">
        <CardHead title="Server" />
        <CardContent className="grid grid-cols-2 gap-6 p-4">
          <KeyValueList
            items={[
              { label: "Version", value: s?.version ?? "-" },
              { label: "URL", value: srv?.url ?? "-" },
              { label: "Base path", value: srv ? srv.base_path || "/" : "-" },
              { label: "PID", value: s ? String(s.pid) : "-" },
              { label: "Started", value: srv ? formatTime(srv.started_at) : "-" },
            ]}
          />
          <KeyValueList
            items={[
              {
                label: "Home",
                value: (
                  <span className="flex items-center gap-2">
                    <span className="truncate">{s?.home ?? "-"}</span>
                    {homeFromEnv ? <Chip>CEREYAN_HOME</Chip> : null}
                  </span>
                ),
              },
              { label: "Served directory", value: s?.served_dir ?? "-" },
              { label: "Config file", value: e ? (e.runtime.config_file ?? "none") : "-" },
              {
                label: "Python",
                value: e ? [e.runtime.python_version, e.runtime.python].filter(Boolean).join(" · ") : "-",
              },
              { label: "Platform", value: e?.runtime.platform ?? "-" },
            ]}
          />
        </CardContent>
      </Card>
      <ConfigurationCard entries={e?.configuration ?? []} toml={e?.cereyan_toml ?? null} loading={!e} />
      <VariablesCard variables={e?.variables ?? []} unset={e?.cereyan_unset ?? []} loading={!e} />
    </div>
  );
}

function ConfigurationCard({
  entries,
  toml,
  loading,
}: {
  entries: Entry[];
  toml: string | null;
  loading: boolean;
}) {
  const [showDefaults, setShowDefaults] = useState(true);
  const [view, setView] = useState("resolved");
  const visible = entries.filter((e) => showDefaults || e.source !== "default");
  const hasEmail = entries.some((e) => e.table === "email");
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <div className="flex items-center justify-between gap-4 border-b py-2 pr-3 pl-4">
        <span className="font-semibold">Configuration</span>
        <div className="flex items-center gap-4">
          <span className="flex items-center gap-2 text-xs text-muted-foreground">
            Show defaults
            <Switch checked={showDefaults} onCheckedChange={setShowDefaults} aria-label="Show defaults" />
          </span>
          <Segmented
            label="Configuration view"
            items={[
              { value: "resolved", label: "Resolved" },
              { value: "file", label: "cereyan.toml" },
            ]}
            value={view}
            onChange={setView}
          />
        </div>
      </div>
      {view === "file" ? (
        toml ? (
          <pre className="overflow-x-auto p-4 font-mono text-xs leading-5" data-testid="cereyan-toml">
            {toml}
          </pre>
        ) : (
          <p className="p-4 text-muted-foreground">No cereyan.toml in the served directory.</p>
        )
      ) : (
        <>
          <div className="flex flex-wrap items-center gap-1.5 border-b px-4 py-2 text-xs text-muted-foreground">
            <span>The first that sets a key wins:</span>
            <Chip>--flag</Chip>
            <span>›</span>
            <Chip>CEREYAN_*</Chip>
            <span>›</span>
            <Chip>app.serve()</Chip>
            <span>›</span>
            <Chip>cereyan.toml</Chip>
            <span>›</span>
            <span>default</span>
          </div>
          <table className="w-full table-fixed text-sm">
            <colgroup>
              <col className="w-56" />
              <col />
              <col className="w-56" />
            </colgroup>
            <thead>
              <tr className="border-b">
                <th className="h-8 px-4 text-left text-xs font-medium">Key</th>
                <th className="px-4 text-left text-xs font-medium">Value</th>
                <th className="px-4 text-left text-xs font-medium">Source</th>
              </tr>
            </thead>
            <tbody>
              {loading ? (
                <tr>
                  <td colSpan={3} className="px-4 py-3 text-muted-foreground">
                    Loading
                  </td>
                </tr>
              ) : null}
              {TABLES.map((table) => {
                const rows = visible.filter((e) => e.table === table);
                const noEmail = table === "email" && !hasEmail && showDefaults && !loading;
                if (!rows.length && !noEmail) return null;
                return (
                  <Fragment key={table}>
                    <tr className="border-b bg-muted/50">
                      <td colSpan={3} className="h-7 px-4 font-mono text-xs text-muted-foreground">
                        [{table}]
                      </td>
                    </tr>
                    {rows.map((e) => (
                      <tr
                        key={`${e.table}.${e.key}`}
                        className="border-b"
                        data-testid={`config-${e.table}.${e.key}`}
                      >
                        <td className="h-[30px] truncate px-4 font-mono text-xs">{e.key}</td>
                        <td className="truncate px-4 font-mono text-xs">{value(e)}</td>
                        <td className="px-4">
                          <Source entry={e} />
                        </td>
                      </tr>
                    ))}
                    {noEmail ? (
                      <tr>
                        <td colSpan={3} className="h-[30px] px-4 text-muted-foreground">
                          Not configured. Rules can't send email until [email] sets host and from.
                        </td>
                      </tr>
                    ) : null}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        </>
      )}
    </Card>
  );
}

function VariablesCard({
  variables,
  unset,
  loading,
}: {
  variables: Variable[];
  unset: string[];
  loading: boolean;
}) {
  const [filter, setFilter] = useState("");
  const [scope, setScope] = useState("all");
  const [expanded, setExpanded] = useState(false);
  const needle = filter.trim().toLowerCase();
  const matches = (v: Variable) => v.name.toLowerCase().includes(needle);
  const cereyan = variables.filter((v) => v.name.startsWith("CEREYAN_"));
  const others = variables.filter((v) => !v.name.startsWith("CEREYAN_"));
  const hidden = variables.filter((v) => v.hidden).length;
  const shownOthers = others.filter(matches);
  const listed = expanded || needle ? shownOthers : shownOthers.slice(0, PROCESS_PREVIEW);
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <CardHead
        title="Environment variables"
        aside={
          <>
            Engines inherit these, plus <code className="font-mono">CEREYAN_HOME</code> and{" "}
            <code className="font-mono">CEREYAN_ENGINE_ID</code>
          </>
        }
      />
      <div className="flex items-center gap-3 border-b px-4 py-3">
        <Input
          aria-label="Filter variables"
          placeholder="Filter by name"
          className="w-72"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <Segmented
          label="Variables shown"
          items={[
            {
              value: "cereyan",
              label: (
                <>
                  Cereyan <span className="ml-1 font-mono font-normal">{cereyan.length}</span>
                </>
              ),
            },
            {
              value: "all",
              label: (
                <>
                  All <span className="ml-1 font-mono font-normal">{variables.length}</span>
                </>
              ),
            },
          ]}
          value={scope}
          onChange={setScope}
        />
        <span className="ml-auto text-xs text-muted-foreground">
          {hidden} {hidden === 1 ? "value" : "values"} hidden
        </span>
      </div>
      <table className="w-full table-fixed text-sm">
        <colgroup>
          <col className="w-64" />
          <col />
        </colgroup>
        <tbody>
          {loading ? (
            <tr>
              <td colSpan={2} className="px-4 py-3 text-muted-foreground">
                Loading
              </td>
            </tr>
          ) : null}
          <tr className="border-b bg-muted/50">
            <td colSpan={2} className="h-7 px-4 text-xs text-muted-foreground">
              Cereyan · {cereyan.length} set
            </td>
          </tr>
          {cereyan.filter(matches).map((v) => (
            <VariableRow key={v.name} variable={v} />
          ))}
          {unset.length ? (
            <tr className="border-b">
              <td colSpan={2} className="px-4 py-2 text-xs leading-[18px] text-muted-foreground">
                Not set: <span className="font-mono">{unset.join(", ")}</span>
              </td>
            </tr>
          ) : null}
          {scope === "all" ? (
            <>
              <tr className="border-b bg-muted/50">
                <td colSpan={2} className="h-7 px-4 text-xs text-muted-foreground">
                  Process · {others.length}
                </td>
              </tr>
              {listed.map((v) => (
                <VariableRow key={v.name} variable={v} />
              ))}
              {listed.length < shownOthers.length ? (
                <tr>
                  <td colSpan={2} className="h-9 px-4">
                    <button
                      type="button"
                      className="cursor-pointer text-xs font-medium hover:underline"
                      onClick={() => setExpanded(true)}
                    >
                      Show {shownOthers.length - listed.length} more
                    </button>
                  </td>
                </tr>
              ) : null}
            </>
          ) : null}
        </tbody>
      </table>
    </Card>
  );
}

function VariableRow({ variable: v }: { variable: Variable }) {
  return (
    <tr className="border-b last:border-b-0">
      <td className="h-[30px] truncate px-4 font-mono text-xs" title={v.name}>
        {v.name}
      </td>
      <td className="truncate px-4 font-mono text-xs" title={v.hidden ? undefined : (v.value ?? "")}>
        {v.hidden ? (
          <>
            <span className="text-muted-foreground">{MASK}</span>
            <span className="ml-2 font-sans text-muted-foreground">hidden</span>
          </>
        ) : (
          v.value
        )}
      </td>
    </tr>
  );
}
