import { render, screen } from "@testing-library/react";
import { ArtifactView } from "./artifacts";
import { dependencyEdges } from "./flow-graph";
import { emptyRule, summarizeDo, summarizeWhen } from "./rule-form";

test("artifact renderers", () => {
  const base = {
    id: 1,
    external_id: "x",
    run_id: 1,
    task_run_id: null,
    key: null,
    created_at: 0,
    updated_at: 0,
  };
  const { rerender } = render(
    <ArtifactView artifact={{ ...base, kind: "progress", data: { value: 60 } } as any} />,
  );
  expect(screen.getByTestId("artifact-progress")).toHaveTextContent("60%");
  rerender(
    <ArtifactView
      artifact={{ ...base, kind: "table", data: { columns: ["a"], rows: [{ a: 1 }, { a: 2 }] } } as any}
    />,
  );
  expect(screen.getByTestId("artifact-table").querySelectorAll("tbody tr").length).toBe(2);
  rerender(<ArtifactView artifact={{ ...base, kind: "markdown", data: { text: "# Title" } } as any} />);
  expect(screen.getByText("Title").tagName).toBe("H1");
  rerender(
    <ArtifactView artifact={{ ...base, kind: "link", data: { url: "https://x", text: "site" } } as any} />,
  );
  expect(screen.getByText("site")).toHaveAttribute("href", "https://x");
});

test("rule summaries and defaults", () => {
  const rule = emptyRule();
  expect(summarizeWhen(rule.when)).toBe("run.failed");
  expect(summarizeWhen({ events: ["run.*"], flows: ["etl"], tags: [], states: [], project: null })).toBe(
    "run.* · flows etl",
  );
  expect(
    summarizeDo([{ kind: "run_flow", flow: "cleanup" } as any, { kind: "email", to: ["a@b"] } as any]),
  ).toBe("run cleanup → email a@b");
});

test("flow dependency edges", () => {
  const flows = [
    { id: 1, name: "daily_sales", project: "p", triggered_by: null },
    { id: 2, name: "build_report", project: "p", triggered_by: "daily_sales" },
    { id: 3, name: "other", project: "q", triggered_by: "daily_sales" },
    { id: 4, name: "inventory", project: "p", triggered_by: null },
    {
      id: 5,
      name: "report",
      project: "p",
      triggered_by: "daily_sales",
      upstreams: ["daily_sales", "inventory"],
      batch_key: "day",
    },
  ] as any[];
  const edges = dependencyEdges(flows);
  expect(edges.length).toBe(3);
  expect(edges[0].from.name).toBe("daily_sales");
  expect(edges[0].to.name).toBe("build_report");
  const intoReport = edges.filter((e) => e.to.name === "report").map((e) => e.from.name);
  expect(intoReport).toEqual(["daily_sales", "inventory"]);
});
