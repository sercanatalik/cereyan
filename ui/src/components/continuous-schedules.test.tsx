// The Continuous tab of the schedule editor and the loop card on the flow page.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import type { ScheduleRow } from "@/api/client";
import { LoopCard } from "@/routes/flows.$flowId";
import { describeDisableAfter, describeSchedule, ScheduleEditor } from "./schedule-editor";

const NOW = Date.now() * 1000;

function withRouter(element: React.ReactNode) {
  const root = createRootRoute();
  const index = createRoute({ getParentRoute: () => root, path: "/", component: () => <>{element}</> });
  const queue = createRoute({
    getParentRoute: () => root,
    path: "/queue",
    component: () => <div>queue</div>,
  });
  const router = createRouter({
    routeTree: root.addChildren([index, queue]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

function row(extra: Partial<ScheduleRow>): ScheduleRow {
  return {
    id: 7,
    external_id: "s7",
    flow_id: 1,
    schedule: { kind: "continuous", delay: 1800 },
    catchup: "skip",
    catchup_max: 100,
    active: true,
    source: "code",
    persist: false,
    created_at: NOW,
    updated_at: NOW,
    next_fire: NOW + 23 * 60_000_000,
    skipped: 0,
    loop_state: "waiting",
    ...extra,
  } as ScheduleRow;
}

test("the Continuous tab asks for the wait, hides catch-up, and saves a continuous body", async () => {
  const onSave = vi.fn();
  const preview = vi.fn(async () => ({ next: [] }));
  render(<ScheduleEditor onSave={onSave} preview={preview} disableAfter={[3, null, 21600]} />);
  fireEvent.click(screen.getByRole("tab", { name: "Continuous" }));
  expect(screen.queryByLabelText("Catch-up")).toBeNull();
  expect(screen.queryByLabelText("Timezone")).toBeNull();
  expect(screen.getByTestId("disable-after-note")).toHaveTextContent(
    "pauses the loop after 3 failures in a row and resumes it 6 h later",
  );
  expect(screen.getByTestId("loop-preview")).toBeInTheDocument();
  expect(screen.getByTestId("next-fires")).toHaveTextContent("Runs again 30 min after each run ends");
  fireEvent.change(screen.getByLabelText("Unit"), { target: { value: "3600" } });
  fireEvent.change(screen.getByLabelText("Wait after each run"), { target: { value: "2" } });
  fireEvent.click(screen.getByText("Save schedule"));
  await waitFor(() =>
    expect(onSave).toHaveBeenCalledWith({ kind: "continuous", delay: 7200, jitter: 0, start_deadline: 0 }),
  );
});

test("a flow without disable_after says the loop keeps going", () => {
  expect(describeDisableAfter(null)).toContain("keeps going");
  expect(describeDisableAfter([3, 3600, 86400])).toContain("3 failures within 1 h");
  expect(describeSchedule(row({}))).toBe("continuous, 30 min after each run");
});

test("a waiting loop counts down and joins the line on request", async () => {
  const onJoinNow = vi.fn();
  const onPause = vi.fn();
  withRouter(
    <LoopCard s={row({})} recent={[]} onPause={onPause} onResume={() => {}} onJoinNow={onJoinNow} />,
  );
  const card = await screen.findByTestId("loop-card");
  expect(card.dataset.state).toBe("waiting");
  expect(card).toHaveTextContent("Joins the line in 2");
  fireEvent.click(screen.getByText("Join the line now"));
  fireEvent.click(screen.getByText("Pause loop"));
  expect(onJoinNow).toHaveBeenCalled();
  expect(onPause).toHaveBeenCalled();
});

test("a loop in line links to the queue", async () => {
  withRouter(
    <LoopCard
      s={row({ loop_state: "in_line", next_fire: null })}
      recent={[]}
      onPause={() => {}}
      onResume={() => {}}
      onJoinNow={() => {}}
    />,
  );
  const card = await screen.findByTestId("loop-card");
  expect(card).toHaveTextContent("In line");
  expect(screen.getByText("Open the queue")).toHaveAttribute("href", "/queue");
  expect(screen.queryByText("Join the line now")).toBeNull();
});

test("a loop stopped by disable_after shows the runs and resumes now", async () => {
  const onResume = vi.fn();
  withRouter(
    <LoopCard
      s={row({
        active: false,
        loop_state: "paused",
        paused_reason: "disabled",
        paused_until: NOW + 3_600_000_000,
      })}
      recent={[
        [3, "Failed", "c", null],
        [2, "Failed", "b", null],
        [1, "Completed", "a", null],
      ]}
      onPause={() => {}}
      onResume={onResume}
      onJoinNow={() => {}}
    />,
  );
  const card = await screen.findByTestId("loop-card");
  expect(card.dataset.state).toBe("disabled");
  expect(card).toHaveTextContent("Paused after failures");
  expect(card).toHaveTextContent("resumes by itself at");
  fireEvent.click(screen.getByText("Resume now"));
  expect(onResume).toHaveBeenCalled();
});
