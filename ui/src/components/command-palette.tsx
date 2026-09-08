import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { api, unwrap } from "@/api/client";
import { StateDot } from "@/components/ported/state-badge";
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { NAV } from "@/lib/nav";

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return debounced;
}

/** ⌘K: jump to a section, a flow, a run by name, or an artifact by key. */
export function CommandPalette({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const navigate = useNavigate();
  const [query, setQuery] = useState("");
  const q = useDebounced(query.trim(), 150);
  useEffect(() => {
    if (!open) setQuery("");
  }, [open]);
  const flows = useQuery({
    queryKey: ["flows"],
    queryFn: async () => unwrap(await api.GET("/api/flows")),
    enabled: open,
  });
  const runs = useQuery({
    queryKey: ["runs", "palette", q],
    queryFn: async () => unwrap(await api.GET("/api/runs", { params: { query: { name: q, limit: 10 } } })),
    enabled: open && q.length > 0,
  });
  const artifacts = useQuery({
    queryKey: ["artifacts", "palette", q],
    queryFn: async () => unwrap(await api.GET("/api/artifacts", { params: { query: { key: q, limit: 5 } } })),
    enabled: open && /^[\w.-]{2,}$/.test(q),
  });
  const go = (fn: () => void) => {
    onOpenChange(false);
    fn();
  };
  const lower = q.toLowerCase();
  const matchingFlows = (flows.data ?? [])
    .filter((f) => !lower || `${f.project}/${f.name}`.toLowerCase().includes(lower))
    .slice(0, 8);
  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Jump to"
      description="Sections, flows, runs, and artifacts"
      showCloseButton={false}
    >
      <CommandInput placeholder="Search runs, flows, artifacts" value={query} onValueChange={setQuery} />
      <CommandList>
        <CommandEmpty>Nothing matches.</CommandEmpty>
        <CommandGroup heading="Go to">
          {NAV.map((item) => (
            <CommandItem
              key={item.to}
              value={`go ${item.label}`}
              onSelect={() => go(() => navigate({ to: item.to }))}
            >
              <item.icon className="size-4 text-muted-foreground" />
              {item.label}
            </CommandItem>
          ))}
        </CommandGroup>
        {matchingFlows.length ? (
          <CommandGroup heading="Flows">
            {matchingFlows.map((f) => (
              <CommandItem
                key={f.id}
                value={`flow ${f.project}/${f.name}`}
                onSelect={() =>
                  go(() => navigate({ to: "/flows/$flowId", params: { flowId: String(f.id) } }))
                }
              >
                <span className="text-muted-foreground">{f.project}/</span>
                {f.name}
              </CommandItem>
            ))}
          </CommandGroup>
        ) : null}
        {runs.data?.items.length ? (
          <CommandGroup heading="Runs">
            {runs.data.items.map((r) => (
              <CommandItem
                key={r.id}
                value={`run ${r.name} ${r.id}`}
                onSelect={() => go(() => navigate({ to: "/runs/$runId", params: { runId: String(r.id) } }))}
              >
                <StateDot type={r.state.type} />
                {r.name}
                <span className="ml-auto text-xs text-muted-foreground">
                  {r.project}/{r.flow_name}
                </span>
              </CommandItem>
            ))}
          </CommandGroup>
        ) : null}
        {artifacts.data?.items.length ? (
          <CommandGroup heading="Artifacts">
            {artifacts.data.items.map((a) => (
              <CommandItem
                key={a.id}
                value={`artifact ${a.key} ${a.id}`}
                onSelect={() =>
                  go(() => navigate({ to: "/artifacts", search: { key: a.key ?? undefined } as any }))
                }
              >
                <span className="font-mono text-xs">{a.key}</span>
                <span className="ml-auto text-xs text-muted-foreground">{a.kind}</span>
              </CommandItem>
            ))}
          </CommandGroup>
        ) : null}
      </CommandList>
    </CommandDialog>
  );
}
