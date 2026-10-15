import { createFileRoute } from "@tanstack/react-router";
import { AgentEditor } from "@/components/agent-editor";
import { Page } from "@/components/shell";

export const Route = createFileRoute("/agents/new")({ component: NewAgentPage });

function NewAgentPage() {
  return (
    <Page crumbs={[{ label: "Agents", to: "/agents" }, { label: "New agent" }]}>
      <h1 className="text-xl font-semibold tracking-tight">New agent</h1>
      <AgentEditor />
    </Page>
  );
}
