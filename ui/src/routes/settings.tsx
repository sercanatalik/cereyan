import { createFileRoute } from "@tanstack/react-router";
import { DataTab } from "@/components/settings/data-tab";
import { EnvironmentTab } from "@/components/settings/environment-tab";
import { GeneralTab } from "@/components/settings/general-tab";
import { Page } from "@/components/shell";
import { UnderlineTabs } from "@/components/ui/underline-tabs";

type Search = { tab?: "environment" | "data" };

const TABS = [
  { value: "general", label: "General" },
  { value: "environment", label: "Environment" },
  { value: "data", label: "Data" },
];

export const Route = createFileRoute("/settings")({
  // General is the default and leaves the URL bare.
  validateSearch: (s: Record<string, unknown>): Search => ({
    tab: s.tab === "environment" || s.tab === "data" ? s.tab : undefined,
  }),
  component: SettingsPage,
});

function SettingsPage() {
  const { tab = "general" } = Route.useSearch();
  const navigate = Route.useNavigate();
  return (
    <Page crumbs={[{ label: "Settings" }]} title="Settings">
      <UnderlineTabs
        items={TABS}
        value={tab}
        onChange={(next) =>
          navigate({ search: { tab: next === "environment" || next === "data" ? next : undefined } })
        }
      />
      {tab === "environment" ? <EnvironmentTab /> : tab === "data" ? <DataTab /> : <GeneralTab />}
    </Page>
  );
}
