import { useQuery } from "@tanstack/react-query";
import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";

export type Approval = components["schemas"]["ApprovalRow"];

// The generated schema marks every field of a definition optional, because the
// server fills defaults; the UI works with the filled shape.
export type ToolSpec = { kind: string; name: string; options: Record<string, unknown> };
export type AgentDefinition = {
  name: string;
  project: string;
  group: string | null;
  description: string;
  model: string;
  api_key: string | null;
  instructions: string;
  prompt: string;
  tools: ToolSpec[];
  policy: { approvals: "ask" | "approve" | "deny"; once: string[] };
  limits: {
    tokens: number | null;
    requests: number | null;
    cost_usd: number | null;
    timeout_seconds: number | null;
  };
  output: { kind: "text" | "json"; schema: Record<string, unknown> | null };
  crash_retries: number;
  enabled: boolean;
};
export type Agent = Omit<components["schemas"]["AgentResponse"], "definition"> & {
  definition: AgentDefinition;
};

/** The catalogue: what agents on this server may be given. Untyped on the wire. */
export type Catalogue = {
  installed: boolean;
  version: string | null;
  capabilities: { name: string; description: string; needs: string[]; options: any; ready: boolean }[];
  common: { name: string; description: string; needs: string[]; options: any; ready: boolean }[];
  mcp: { ready: boolean; needs: string[] };
  providers: { id: string; env: string; installed: boolean }[];
  self: { ready: boolean; tools: { name: string; description: string; read_only: boolean }[] };
  allowlist: { mcp_commands: string[]; mcp_urls: string[]; cost_per_day: number };
  features: Record<string, unknown>;
};

export function useAgents() {
  return useQuery({
    queryKey: ["agents"],
    queryFn: async () => unwrap(await api.GET("/api/agents")) as unknown as Agent[],
  });
}

export function useAgent(id: number) {
  return useQuery({
    queryKey: ["agent", id],
    queryFn: async () =>
      unwrap(await api.GET("/api/agents/{id}", { params: { path: { id } } })) as unknown as Agent,
  });
}

export function useCatalogue() {
  return useQuery({
    queryKey: ["agents", "catalogue"],
    queryFn: async () => unwrap(await api.GET("/api/agents/catalogue")) as unknown as Catalogue,
  });
}

/** Whether this server shows agents: a host has the extra, or an agent exists. */
export function useAgentsSupported(): boolean {
  const server = useQuery({
    queryKey: ["server"],
    queryFn: async () => unwrap(await api.GET("/api/server")),
    refetchInterval: 5000,
  });
  const agents = useAgents();
  const features = (server.data?.features ?? {}) as { pydantic_ai?: string | null };
  return Boolean(features.pydantic_ai) || (agents.data?.length ?? 0) > 0;
}

export const EMPTY_DEFINITION: AgentDefinition = {
  name: "",
  project: "agents",
  group: null,
  description: "",
  model: "openai:gpt-5",
  api_key: null,
  instructions: "",
  prompt: "",
  tools: [],
  policy: { approvals: "ask", once: [] },
  limits: { tokens: 50000, requests: 20, cost_usd: 0.5, timeout_seconds: 600 },
  output: { kind: "text", schema: null },
  crash_retries: 2,
  enabled: true,
};

/** A tool as a short label: its name, with what marks it. */
export function toolTags(tool: ToolSpec, definition: AgentDefinition): string[] {
  const tags: string[] = [];
  const names =
    tool.kind === "self" ? (((tool.options as { tools?: string[] }).tools ?? []) as string[]) : [tool.name];
  if (names.some((n) => definition.policy.once.includes(n))) tags.push("once");
  if (tool.kind === "self" && names.length)
    tags.push(names.length === 1 ? names[0] : `${names.length} tools`);
  if (tool.kind === "mcp") tags.push((tool.options as { command?: string }).command ? "stdio" : "http");
  return tags;
}

export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;
  return String(n);
}

export function formatCost(usd: number | null | undefined): string {
  if (usd == null) return "—";
  return usd < 0.01 && usd > 0 ? "<$0.01" : `$${usd.toFixed(2)}`;
}
