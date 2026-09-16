import { fireEvent, render, screen, within } from "@testing-library/react";
import { Fragment, useState } from "react";
import { expect, test } from "vitest";
import {
  GroupCount,
  GroupSection,
  GroupStateRollup,
  openSections,
  useGroupOpen,
} from "@/components/grouped-rows";
import { Table, Td } from "@/components/ui/table";
import { groupOf, groupOptions, nestByProject } from "@/lib/groups";

type Row = { id: number; project: string; group?: string | null; name: string };

const row = (id: number, project: string, group?: string | null): Row => ({
  id,
  project,
  group: group ?? null,
  name: `row-${id}`,
});

/** A minimal table using the same two-level ladder as the real pages. */
function Harness({ rows, search = false }: { rows: Row[]; search?: boolean }) {
  const [q, setQ] = useState(search);
  const projects = nestByProject(rows);
  const open = useGroupOpen(openSections(projects), q);
  const cells = (r: Row) => (
    <tr key={r.id}>
      <Td>{r.name}</Td>
      <Td />
    </tr>
  );
  return (
    <>
      <button type="button" onClick={() => setQ((v) => !v)}>
        toggle search
      </button>
      <Table>
        {projects.map((p) => (
          <Fragment key={p.openKey}>
            <GroupSection
              name={p.key}
              testId={`project-${p.key}`}
              open={open.isOpen(p.openKey)}
              onOpenChange={(next) => open.toggle(p.openKey, next)}
              rollup={
                <Td>
                  <GroupCount shown={p.items.length} />
                </Td>
              }
            >
              {p.rows.map(cells)}
            </GroupSection>
            {open.isOpen(p.openKey)
              ? p.groups.map((g) => (
                  <GroupSection
                    key={g.openKey}
                    name={g.key}
                    testId={`group-${p.key}/${g.key}`}
                    indent
                    open={open.isOpen(g.openKey)}
                    onOpenChange={(next) => open.toggle(g.openKey, next)}
                    rollup={
                      <Td>
                        <GroupCount shown={g.items.length} />
                      </Td>
                    }
                  >
                    {g.items.map(cells)}
                  </GroupSection>
                ))
              : null}
          </Fragment>
        ))}
      </Table>
    </>
  );
}

const isOpen = (testId: string) => screen.getByTestId(testId).getAttribute("data-open") === "true";

test("rows nest under their project, and a group equal to the project is the project's own rows", () => {
  expect(groupOf({ project: "warehouse", group: null })).toBe("warehouse");
  expect(groupOf({ project: "warehouse", group: "nightly" })).toBe("nightly");
  const projects = nestByProject([
    row(1, "warehouse", "nightly"),
    row(2, "analytics", "nightly"),
    row(3, "warehouse"),
    row(4, "warehouse", "warehouse"),
  ]);
  // Projects alphabetical, so live updates cannot reshuffle the page.
  expect(projects.map((p) => p.key)).toEqual(["analytics", "warehouse"]);
  // A group name used in two projects appears once under each, not merged.
  expect(projects[0].groups.map((g) => g.key)).toEqual(["nightly"]);
  expect(projects[1].groups.map((g) => g.key)).toEqual(["nightly"]);
  // Declaring the project's own name is the same as declaring nothing.
  expect(projects[1].rows.map((r) => r.id)).toEqual([3, 4]);
  // Every row lands exactly once, so the counts sum to the total.
  expect(projects.reduce((n, p) => n + p.items.length, 0)).toBe(4);
  expect(groupOptions([row(1, "warehouse", "nightly"), row(3, "warehouse")])).toEqual([
    "nightly",
    "warehouse",
  ]);
});

test("more than five rows starts collapsed, five or fewer expanded", () => {
  const big = Array.from({ length: 6 }, (_, i) => row(i, "big"));
  const small = Array.from({ length: 5 }, (_, i) => row(100 + i, "small"));
  render(<Harness rows={[...big, ...small]} />);
  expect(isOpen("project-big")).toBe(false);
  expect(isOpen("project-small")).toBe(true);
  // Collapsed rows stay in the DOM so `aria-controls` resolves; they are hidden.
  expect(screen.getByText("row-0")).not.toBeVisible();
  expect(screen.getByText("row-100")).toBeVisible();
});

test("a lone project and its lone group are both expanded however large", () => {
  render(<Harness rows={Array.from({ length: 40 }, (_, i) => row(i, "only", "nightly"))} />);
  expect(isOpen("project-only")).toBe(true);
  expect(isOpen("group-only/nightly")).toBe(true);
});

test("a collapsed project omits its groups, its header having rolled them up", () => {
  const rows = [...Array.from({ length: 6 }, (_, i) => row(i, "big", "nightly")), row(90, "other")];
  render(<Harness rows={rows} />);
  expect(isOpen("project-big")).toBe(false);
  expect(screen.queryByTestId("group-big/nightly")).toBeNull();
  // The project's count still covers every row beneath it.
  expect(within(screen.getByTestId("project-big")).getByTestId("group-count")).toHaveTextContent("6");
});

test("one group name in two projects toggles independently", () => {
  render(<Harness rows={[row(1, "warehouse", "nightly"), row(2, "analytics", "nightly")]} />);
  expect(isOpen("group-warehouse/nightly")).toBe(true);
  fireEvent.click(within(screen.getByTestId("group-warehouse/nightly")).getByRole("button"));
  expect(isOpen("group-warehouse/nightly")).toBe(false);
  expect(isOpen("group-analytics/nightly")).toBe(true);
});

test("search expands a collapsed section and clearing it restores the default", () => {
  const rows = [...Array.from({ length: 6 }, (_, i) => row(i, "big")), row(99, "other")];
  render(<Harness rows={rows} />);
  expect(isOpen("project-big")).toBe(false);
  fireEvent.click(screen.getByText("toggle search"));
  expect(isOpen("project-big")).toBe(true);
  fireEvent.click(screen.getByText("toggle search"));
  expect(isOpen("project-big")).toBe(false);
});

test("a section growing past the threshold under a live update stays expanded", () => {
  const rows = [...Array.from({ length: 5 }, (_, i) => row(i, "grow")), row(90, "other")];
  const { rerender } = render(<Harness rows={rows} />);
  expect(isOpen("project-grow")).toBe(true);
  // A sixth row arrives: the default was fixed on first sight and is not recomputed.
  rerender(<Harness rows={[...rows, row(5, "grow")]} />);
  expect(isOpen("project-grow")).toBe(true);
});

test("an explicit toggle wins over the defaults", () => {
  const rows = [row(1, "a"), row(2, "b")];
  const { rerender } = render(<Harness rows={rows} />);
  expect(isOpen("project-a")).toBe(true);
  fireEvent.click(within(screen.getByTestId("project-a")).getByRole("button"));
  expect(isOpen("project-a")).toBe(false);
  // Filtering down to that one project does not reopen what the user closed.
  rerender(<Harness rows={[row(1, "a")]} />);
  expect(isOpen("project-a")).toBe(false);
});

test("the header is a button exposing its state and the rows it controls", () => {
  render(<Harness rows={[row(1, "a"), row(2, "b")]} />);
  const button = within(screen.getByTestId("project-a")).getByRole("button");
  expect(button).toHaveAttribute("aria-expanded", "true");
  const controlled = document.getElementById(button.getAttribute("aria-controls") as string);
  expect(controlled).not.toBeNull();
  expect(controlled).toHaveTextContent("row-1");
  fireEvent.click(button);
  expect(button).toHaveAttribute("aria-expanded", "false");
  expect(controlled).toHaveAttribute("hidden");
});

test("the count reads n of m only while part of the section is hidden", () => {
  const { rerender } = render(<GroupCount shown={3} total={12} />);
  expect(screen.getByTestId("group-count")).toHaveTextContent("3 of 12");
  rerender(<GroupCount shown={12} total={12} />);
  expect(screen.getByTestId("group-count")).toHaveTextContent("12");
});

test("the rollup labels its counts and reports stale flows separately", () => {
  render(<GroupStateRollup counts={{ Completed: 9, Failed: 2, Running: 1 }} stale={3} />);
  const bar = screen.getByTestId("state-bar");
  expect(bar).toHaveAttribute("aria-label", "9 Completed, 2 Failed, 1 Running");
  const rollup = screen.getByTestId("group-rollup");
  expect(rollup).toHaveTextContent("9");
  expect(rollup).toHaveTextContent("2");
  // Registration health is its own channel: an all-green bar must not hide it.
  expect(screen.getByTestId("group-stale")).toHaveTextContent("3 stale");
});

test("a section with no stale flows shows no stale marker", () => {
  render(<GroupStateRollup counts={{ Completed: 4 }} stale={0} />);
  expect(screen.queryByTestId("group-stale")).toBeNull();
});

test("a header at either level spans exactly the table's columns", () => {
  // A header cell too many silently shifts every rollup out of its column.
  render(<Harness rows={[row(1, "a", "nightly"), row(2, "b")]} />);
  const width = (tr: HTMLElement) =>
    Array.from(tr.querySelectorAll(":scope > td")).reduce(
      (n, td) => n + Number(td.getAttribute("colspan") ?? 1),
      0,
    );
  const dataRow = screen.getByText("row-1").closest("tr") as HTMLElement;
  expect(width(screen.getByTestId("project-a"))).toBe(width(dataRow));
  expect(width(screen.getByTestId("group-a/nightly"))).toBe(width(dataRow));
});
