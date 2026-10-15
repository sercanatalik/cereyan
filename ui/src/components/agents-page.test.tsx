import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ProjectProvider } from "@/lib/project";
import { routeTree } from "@/routeTree.gen";
import { Conversation, hasConversation } from "./conversation";

const fetchMock = vi.hoisted(() => {
  const fn = vi.fn();
  globalThis.fetch = fn as unknown as typeof fetch;
  const NativeRequest = globalThis.Request;
  globalThis.Request = class extends NativeRequest {
    constructor(input: RequestInfo | URL, init?: RequestInit) {
      super(typeof input === "string" && input.startsWith("/") ? `http://localhost${input}` : input, init);
    }
  } as typeof Request;
  return fn;
});

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

const DEFINITION = {
  name: "digest",
  project: "examples",
  group: "agents",
  description: "Summarise overnight failures",
  model: "openai:gpt-5",
  api_key: "openai/api_key",
  instructions: "You are the on-call assistant.",
  prompt: "Write the digest for {day}.",
  tools: [
    { kind: "capability", name: "WebSearch", options: {} },
    { kind: "self", name: "cereyan", options: { tools: ["list_runs"] } },
  ],
  policy: { approvals: "ask", once: ["list_runs"] },
  limits: { tokens: 50000, requests: 20, cost_usd: 0.5, timeout_seconds: 600 },
  output: { kind: "text", schema: null },
  crash_retries: 2,
  enabled: true,
};

const AGENT = {
  id: 7,
  external_id: "00000000-0000-4000-8000-000000000007",
  project: "examples",
  name: "digest",
  source: "ui",
  enabled: true,
  flow_id: 42,
  definition: DEFINITION,
  created_at: 1,
  updated_at: 1,
  allow_error: null,
  parameter_schema: { type: "object", properties: { prompt: { type: "string" }, day: { type: "string" } } },
  cost_today: 1.43,
  cost_cap: 20,
  usage_24h: { runs: 3, tokens: 18400, cost_usd: 0.11 },
};

const CATALOGUE = {
  installed: true,
  version: "2.55.0",
  capabilities: [{ name: "WebSearch", description: "Search the web", needs: [], options: {}, ready: true }],
  common: [
    {
      name: "tavily_search",
      description: "Tavily",
      needs: ["secret:tavily/api_key"],
      options: {},
      ready: true,
    },
  ],
  mcp: { ready: false, needs: ["package:pydantic-ai-slim[mcp]"] },
  providers: [{ id: "openai", env: "OPENAI_API_KEY", installed: true }],
  self: {
    ready: true,
    tools: [
      { name: "list_runs", description: "", read_only: true },
      { name: "cancel_run", description: "", read_only: false },
    ],
  },
  allowlist: { mcp_commands: ["npx"], mcp_urls: ["self"], cost_per_day: 20 },
  features: { pydantic_ai: "2.55.0" },
};

const FLOW = {
  id: 42,
  external_id: "00000000-0000-4000-8000-000000000042",
  project: "examples",
  name: "digest",
  module: "cereyan.pydantic_ai.runner",
  source_dir: "/x",
  tags: ["agent"],
  group: "agents",
  parameter_schema: AGENT.parameter_schema,
  options: { agent: true, agent_id: 7 },
  created_at: 1,
  live: true,
  recent_runs: [[1, "Completed", "Completed", 41000000]],
  triggers: [],
  schedules: [
    {
      id: 1,
      flow_id: 42,
      external_id: "x",
      schedule: { kind: "cron", cron: "0 7 * * *", timezone: "UTC" },
      active: true,
      catchup: "skip",
      catchup_max: 100,
      source: "ui",
      persist: false,
      created_at: 1,
      updated_at: 1,
      next_fire: null,
      skipped: 0,
    },
  ],
  health: null,
  triggered_by: null,
  upstreams: [],
  batch_key: null,
};

let agents: any[] = [AGENT];
let posted: any[] = [];

beforeEach(() => {
  localStorage.clear();
  agents = [AGENT];
  posted = [];
  fetchMock.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = input instanceof Request ? input : null;
    const url = new URL(request ? request.url : String(input), "http://x");
    const p = url.pathname;
    const method = request?.method ?? init?.method ?? "GET";
    if (p === "/api/agents" && method === "POST") {
      posted.push(JSON.parse(await (request as Request).text()));
      return json({ ...AGENT, id: 8, name: posted[0].name }, 201);
    }
    if (p === "/api/agents") return json(agents);
    if (p === "/api/agents/catalogue") return json(CATALOGUE);
    if (p === "/api/agents/7") return json(AGENT);
    if (p === "/api/agents/7/approvals")
      return json([
        {
          id: 1,
          agent_id: 7,
          tool: "cancel_run",
          args_hash: "h",
          args: { run_id: 3 },
          first_run_id: 5,
          created_at: 1,
        },
      ]);
    if (p === "/api/flows") return json([FLOW]);
    if (p === "/api/flows/42") return json(FLOW);
    if (p === "/api/runs")
      return json({
        items: [
          {
            id: 9,
            external_id: "r",
            name: "quiet-heron",
            flow_id: 42,
            flow_name: "digest",
            project: "examples",
            parameters: {},
            state: { type: "Completed", name: "Completed", timestamp: 1 },
            created_at: 1,
            created_by: "schedule",
            attributes: { outcome: "posted · 2 failures", tokens: 14200 },
            failure_count: 0,
            crash_count: 0,
            tags: [],
          },
        ],
        next_cursor: null,
      });
    if (p === "/api/variables")
      return json([{ name: "openai/api_key", value: "********", tags: [], secret: true, created_at: 1 }]);
    if (p === "/api/settings") return json({ engine_saturation_risk: false, saturation_flows: [] });
    if (p === "/api/server")
      return json({
        title: "cereyan",
        queued: 0,
        engines: [],
        paused: null,
        exposed: false,
        auth: false,
        features: { pydantic_ai: "2.55.0" },
      });
    if (p === "/api/counts") return json({ runs: {}, task_runs: {}, active: 0, flows: {} });
    return json({ error: `unmocked ${p}` }, 404);
  });
});

function mount(path: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree,
    history: createMemoryHistory({ initialEntries: [path] }),
    context: { queryClient: client },
  });
  render(
    <QueryClientProvider client={client}>
      <ProjectProvider>
        <RouterProvider router={router} />
      </ProjectProvider>
    </QueryClientProvider>,
  );
}

test("the agents list shows each agent with its model, tools, schedule and spend", async () => {
  mount("/agents");
  const row = await screen.findByTestId("agent-row");
  expect(within(row).getByTestId("agent-name")).toHaveTextContent("digest");
  expect(row).toHaveTextContent("openai:gpt-5");
  expect(row).toHaveTextContent("WebSearch");
  expect(row).toHaveTextContent("list_runs");
  expect(row).toHaveTextContent("once");
  expect(row).toHaveTextContent("18.4k");
  expect(screen.getByTestId("agent-group")).toHaveTextContent("examples › agents");
  // The sidebar lists Agents, since the server reports the extra.
  expect(await screen.findByRole("link", { name: /Agents/ })).toBeInTheDocument();
});

test("the editor saves a definition with the ticked tools and policy", async () => {
  mount("/agents/new");
  await screen.findByTestId("agent-editor");
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "triage" } });
  fireEvent.change(screen.getByRole("textbox", { name: "Instructions" }), {
    target: { value: "Refund broken orders." },
  });
  await screen.findByTestId("tool-capability-WebSearch");
  fireEvent.click(within(screen.getByTestId("tool-capability-WebSearch")).getByRole("checkbox"));
  // Commands come from the allowlist only.
  expect(screen.getByTestId("add-mcp")).toHaveTextContent("npx");
  expect(screen.getByTestId("add-mcp")).toHaveTextContent("cereyan.toml");
  fireEvent.click(screen.getByLabelText("Deny"));
  fireEvent.click(screen.getByTestId("save-agent"));
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0].name).toBe("triage");
  expect(posted[0].tools).toEqual([{ kind: "capability", name: "WebSearch", options: {} }]);
  expect(posted[0].policy.approvals).toBe("deny");
});

test("the agent page shows the definition, spend, runs with outcomes, and remembered approvals", async () => {
  mount("/agents/7");
  expect(await screen.findByRole("heading", { name: "digest" })).toBeInTheDocument();
  expect(screen.getByTestId("cost-today")).toHaveTextContent("$1.43 of $20.00 cap");
  const run = await screen.findByTestId("agent-run");
  expect(run).toHaveTextContent("posted · 2 failures");
  expect(run).toHaveTextContent("14.2k");
  expect(await screen.findByTestId("approval")).toHaveTextContent('cancel_run({"run_id":3})');
  expect(screen.getByText(/At 07:00|7:00/)).toBeInTheDocument();
});

test("a conversation is built from the unit summaries with replay and in-doubt marks", () => {
  const run = {
    id: 1,
    parameters: { prompt: "Refund order 1042." },
    state: {
      type: "Paused",
      name: "Paused",
      message: "x",
      details: {
        prompt: "Did email_customer go out?",
        schema: { type: "object", properties: { action: { enum: ["happened", "run_again", "fail"] } } },
      },
      timestamp: 1,
    },
  } as any;
  const tasks = [
    {
      id: 1,
      dynamic_key: "payments__model.request-0",
      state: {
        type: "Completed",
        name: "Replayed",
        timestamp: 1,
        details: {
          summary: {
            kind: "model",
            agent: "payments",
            model: "gpt-5",
            usage: { input: 1200, output: 60 },
            text: "",
            tool_calls: [{ tool: "refund", args: { order_id: "1042" } }],
          },
        },
      },
    },
    {
      id: 2,
      dynamic_key: "payments__function_toolset__<agent>.call_tool:refund-0",
      once: true,
      state: {
        type: "Completed",
        name: "Recovered",
        timestamp: 1,
        details: {
          summary: {
            kind: "tool",
            agent: "payments",
            tool: "refund",
            args: { order_id: "1042" },
            result: "re_1042_3900",
            result_kind: "tool_return",
          },
        },
      },
    },
    {
      id: 3,
      dynamic_key: "payments__function_toolset__<agent>.call_tool:email_customer-0",
      once: true,
      state: {
        type: "Completed",
        name: "Completed",
        timestamp: 1,
        details: {
          in_doubt: "deferred",
          summary: {
            kind: "tool",
            agent: "payments",
            tool: "email_customer",
            args: { order_id: "1042" },
            result: "",
            result_kind: "approval_required",
          },
        },
      },
    },
  ] as any[];
  expect(hasConversation(tasks)).toBe(true);
  expect(
    hasConversation([{ id: 4, state: { type: "Completed", name: "Completed", details: {} } }] as any),
  ).toBe(false);
  const client = new QueryClient();
  render(
    <QueryClientProvider client={client}>
      <Conversation run={run} tasks={tasks} />
    </QueryClientProvider>,
  );
  expect(screen.getByText("Refund order 1042.")).toBeInTheDocument();
  expect(screen.getByText("replayed · not billed")).toBeInTheDocument();
  expect(screen.getByText("recovered by probe")).toBeInTheDocument();
  expect(screen.getByText("in doubt · deferred to you")).toBeInTheDocument();
  expect(screen.getByText(/→ refund\(order_id="1042"\)/)).toBeInTheDocument();
  expect(screen.getByTestId("conversation-usage")).toHaveTextContent(
    "1 model requests · 1.2k in / 60 out · 60 out replayed, not billed",
  );
  // The question sits inside the conversation, answerable in place.
  expect(screen.getByTestId("paused-banner")).toHaveTextContent("Did email_customer go out?");
});
