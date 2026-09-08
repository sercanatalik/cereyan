import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { PausedBanner } from "./paused-banner";

const run = {
  id: 5,
  name: "calm-otter",
  state: {
    type: "Paused",
    name: "Paused",
    message: "Approve the load?",
    details: {
      prompt: "Approve the load?",
      schema: { properties: { approve: { type: "boolean" } }, required: ["approve"] },
    },
    timestamp: 1,
  },
} as any;

test("paused banner shows the prompt and a form from the schema", () => {
  render(
    <QueryClientProvider client={new QueryClient()}>
      <PausedBanner run={run} />
    </QueryClientProvider>,
  );
  expect(screen.getByTestId("paused-banner")).toHaveTextContent("Approve the load?");
  expect(screen.getByTestId("run-form")).toBeInTheDocument();
  expect(screen.getByText("Resume")).toBeInTheDocument();
});

test("paused banner falls back to a text answer without a schema", () => {
  const plain = { ...run, state: { ...run.state, details: { prompt: "Name?" } } };
  render(
    <QueryClientProvider client={new QueryClient()}>
      <PausedBanner run={plain} />
    </QueryClientProvider>,
  );
  expect(screen.getByLabelText("Answer")).toBeInTheDocument();
  expect(screen.getByTestId("resume-run")).toBeInTheDocument();
});
