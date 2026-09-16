import { render, screen } from "@testing-library/react";
import { StateBadge } from "./state-badge";

test("shows the state name and a coloured dot", () => {
  render(
    <StateBadge state={{ type: "Completed", name: "Skipped", message: null, details: {}, timestamp: 0 }} />,
  );
  const badge = screen.getByText("Skipped");
  expect(badge).toHaveAttribute("data-state", "Completed");
});
