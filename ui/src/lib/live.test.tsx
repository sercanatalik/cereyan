import { QueryClient } from "@tanstack/react-query";
import { applyEvent } from "./live";

test("run.updated patches the run and list caches", () => {
  const client = new QueryClient();
  client.setQueryData(["run", 1], { id: 1, state: { type: "Running" } });
  client.setQueryData(["runs", "list"], {
    items: [{ id: 1, state: { type: "Running" } }, { id: 2 }],
    next_cursor: null,
  });
  applyEvent(client, { kind: "run.updated", seq: 5, data: { id: 1, state: { type: "Completed" } } });
  expect((client.getQueryData(["run", 1]) as any).state.type).toBe("Completed");
  expect((client.getQueryData(["runs", "list"]) as any).items[0].state.type).toBe("Completed");
});

test("task_run.updated appends to the run's task list", () => {
  const client = new QueryClient();
  client.setQueryData(["run-tasks", 7], [{ id: 1, run_id: 7 }]);
  applyEvent(client, {
    kind: "task_run.updated",
    seq: 6,
    data: { id: 2, run_id: 7, state: { type: "Pending" } },
  });
  expect((client.getQueryData(["run-tasks", 7]) as any[]).map((t) => t.id)).toEqual([1, 2]);
});
