import { createFileRoute } from "@tanstack/react-router";
import { AgentEditor } from "@/components/agent-editor";
import { Page } from "@/components/shell";
import { useAgent } from "@/lib/agents";

export const Route = createFileRoute("/agents/$agentId/edit")({ component: EditAgentPage });

function EditAgentPage() {
  const { agentId } = Route.useParams();
  const id = Number(agentId);
  const agent = useAgent(id);
  if (!agent.data) {
    return <Page crumbs={[{ label: "Agents", to: "/agents" }, { label: agentId }]}>Loading</Page>;
  }
  return (
    <Page
      crumbs={[
        { label: "Agents", to: "/agents" },
        { label: agent.data.name, to: `/agents/${id}` },
        { label: "Edit" },
      ]}
    >
      <h1 className="text-xl font-semibold tracking-tight">Edit {agent.data.name}</h1>
      <AgentEditor id={id} initial={agent.data.definition} />
    </Page>
  );
}
