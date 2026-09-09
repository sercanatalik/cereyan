import { fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { expect, test } from "vitest";
import { GroupCount, GroupSection, GroupStateRollup, useGroupOpen } from "@/components/grouped-rows";
import { Table, Td } from "@/components/ui/table";
import { groupBy, groupOf } from "@/lib/groups";

type Row = { id: number; project: string; group?: string | null; name: string };

const row = (id: number, project: string, group?: string | null): Row => ({
  id,
  project,
  group: group ?? null,
  name: `row-${id}`,
});

/** A minimal table using the same open-state ladder as the real pages. */
function Harness({ rows, search = false }: { rows: Row[]; search?: boolean }) {
  const [q, setQ] = useState(search);
  const groups = groupBy(rows);
  const open = useGroupOpen(groups, q);
  return (
    <>
      <button type="button" onClick={() => setQ((v) => !v)}>
        toggle search
      </button>
      <Table>
        {groups.map((g) => (
          <GroupSection
            key={g.key}
            group={g}
            open={open.isOpen(g.key)}
            onOpenChange={(next) => open.toggle(g.key, next)}
            identity={g.spansProjects ? g.projects.join(" · ") : g.items[0]?.project}
            rollup={
              <Td>
                <GroupCount shown={g.items.length} />
              </Td>
            }
          >
            {g.items.map((r) => (
              <tr key={r.id}>
                <Td>{r.name}</Td>
                <Td />
              </tr>
            ))}
          </GroupSection>
        ))}
      </Table>
    </>
  );
}

const isOpen = (key: string) => screen.getByTestId(`group-${key}`).getAttribute("data-open") === "true";

test("group falls back to the project and a declared group spans projects", () => {
  expect(groupOf({ project: "warehouse", group: null })).toBe("warehouse");
  expect(groupOf({ project: "warehouse", group: "nightly" })).toBe("nightly");
  const groups = groupBy([row(1, "warehouse", "nightly"), row(2, "analytics", "nightly"), row(3, "zed")]);
  // Alphabetical, so live updates cannot reshuffle the page.
  expect(groups.map((g) => g.key)).toEqual(["nightly", "zed"]);
  expect(groups[0].projects).toEqual(["analytics", "warehouse"]);
  expect(groups[0].spansProjects).toBe(true);
  expect(groups[1].spansProjects).toBe(false);
});

test("more than five rows starts collapsed, five or fewer expanded", () => {
  const big = Array.from({ length: 6 }, (_, i) => row(i, "big"));
  const small = Array.from({ length: 5 }, (_, i) => row(100 + i, "small"));
  render(<Harness rows={[...big, ...small]} />);
  expect(isOpen("big")).toBe(false);
  expect(isOpen("small")).toBe(true);
  // Collapsed rows stay in the DOM so `aria-controls` resolves; they are hidden.
  expect(screen.getByText("row-0")).not.toBeVisible();
  expect(screen.getByText("row-100")).toBeVisible();
});

test("a lone group is expanded however large", () => {
  render(<Harness rows={Array.from({ length: 40 }, (_, i) => row(i, "only"))} />);
  expect(isOpen("only")).toBe(true);
});

test("search expands a collapsed group and clearing it restores the default", () => {
  const rows = [...Array.from({ length: 6 }, (_, i) => row(i, "big")), row(99, "other")];
  render(<Harness rows={rows} />);
  expect(isOpen("big")).toBe(false);
  fireEvent.click(screen.getByText("toggle search"));
  expect(isOpen("big")).toBe(true);
  fireEvent.click(screen.getByText("toggle search"));
  expect(isOpen("big")).toBe(false);
});

test("a group growing past the threshold under a live update stays expanded", () => {
  const rows = [...Array.from({ length: 5 }, (_, i) => row(i, "grow")), row(90, "other")];
  const { rerender } = render(<Harness rows={rows} />);
  expect(isOpen("grow")).toBe(true);
  // A sixth row arrives: the default was fixed on first sight and is not recomputed.
  rerender(<Harness rows={[...rows, row(5, "grow")]} />);
  expect(isOpen("grow")).toBe(true);
});

test("an explicit toggle wins over the defaults", () => {
  const rows = [row(1, "a"), row(2, "b")];
  const { rerender } = render(<Harness rows={rows} />);
  expect(isOpen("a")).toBe(true);
  fireEvent.click(within(screen.getByTestId("group-a")).getByRole("button"));
  expect(isOpen("a")).toBe(false);
  // Filtering down to that one group does not reopen what the user closed.
  rerender(<Harness rows={[row(1, "a")]} />);
  expect(isOpen("a")).toBe(false);
});

test("the header is a button exposing its state and the rows it controls", () => {
  render(<Harness rows={[row(1, "a"), row(2, "b")]} />);
  const button = within(screen.getByTestId("group-a")).getByRole("button");
  expect(button).toHaveAttribute("aria-expanded", "true");
  const controlled = document.getElementById(button.getAttribute("aria-controls") as string);
  expect(controlled).not.toBeNull();
  expect(controlled).toHaveTextContent("row-1");
  fireEvent.click(button);
  expect(button).toHaveAttribute("aria-expanded", "false");
  expect(controlled).toHaveAttribute("hidden");
});

test("the count reads n of m only while part of the group is hidden", () => {
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

test("a group with no stale flows shows no stale marker", () => {
  render(<GroupStateRollup counts={{ Completed: 4 }} stale={0} />);
  expect(screen.queryByTestId("group-stale")).toBeNull();
});

test("a group header spans exactly the table's columns", () => {
  // A header cell too many silently shifts every rollup out of its column.
  render(<Harness rows={[row(1, "a"), row(2, "b")]} />);
  const header = screen.getByTestId("group-a");
  const width = (tr: HTMLElement) =>
    Array.from(tr.querySelectorAll(":scope > td")).reduce(
      (n, td) => n + Number(td.getAttribute("colspan") ?? 1),
      0,
    );
  const dataRow = screen.getByText("row-1").closest("tr") as HTMLElement;
  expect(width(header)).toBe(width(dataRow));
});
