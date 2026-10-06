import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { fireEvent, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { type RecentRun, RunStrip } from "./run-strip";

function withRouter(node: ReactNode) {
  const router = createRouter({
    routeTree: createRootRoute({ component: () => node }),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  return render(<RouterProvider router={router} />);
}

// Newest first, as the flows API returns them.
const runs: RecentRun[] = [
  [13, "Failed", "Failed", 41_000],
  [12, "Completed", "Completed", 2_000],
  [11, "Completed", "Completed", null],
];

test("fills the slots without a run with neutral squares, newest on the right", async () => {
  withRouter(<RunStrip runs={runs} />);
  const strip = await screen.findByTestId("run-strip");
  const states = [...strip.children].map((c) => c.getAttribute("data-state"));
  expect(states).toEqual([
    "none",
    "none",
    "none",
    "none",
    "none",
    "none",
    "none",
    "Completed",
    "Completed",
    "Failed",
  ]);
});

test("links each run and names its id, state and duration", async () => {
  withRouter(<RunStrip runs={runs} />);
  const link = await screen.findByRole("link", { name: "Run 13 · Failed · 41 ms" });
  expect(link).toHaveAttribute("href", "/runs/13");
});

test("with onSelect, squares are buttons and the selected one is pressed", () => {
  const picked: number[] = [];
  render(<RunStrip runs={runs} selectedId={12} onSelect={(r) => picked.push(r[0])} />);
  const buttons = screen.getAllByRole("button");
  expect(buttons).toHaveLength(3);
  expect(screen.getByRole("button", { name: /Run 12/ })).toHaveAttribute("aria-pressed", "true");
  expect(screen.getByRole("button", { name: /Run 13/ })).toHaveAttribute("aria-pressed", "false");
  fireEvent.click(screen.getByRole("button", { name: /Run 13/ }));
  expect(picked).toEqual([13]);
});
