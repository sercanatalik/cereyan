// The Queue page's opening line, per mix of executing and waiting runs.
import { expect, test } from "vitest";
import { queueSummary } from "./queue";

test("idle", () => {
  expect(queueSummary({ executing: 0, waiting: 0 })).toBe("No run is executing or waiting.");
});

test("executing with nothing waiting", () => {
  expect(queueSummary({ executing: 1, waiting: 0 })).toBe(
    "1 run is executing. Nothing is waiting, so new runs start at once.",
  );
  expect(queueSummary({ executing: 3, waiting: 0 })).toMatch(/^3 runs are executing\./);
});

test("executing with runs waiting", () => {
  expect(queueSummary({ executing: 2, waiting: 1 })).toBe("2 runs are executing. 1 run is waiting in line.");
  expect(queueSummary({ executing: 1, waiting: 4 })).toBe("1 run is executing. 4 runs are waiting in line.");
});

test("waiting with nothing executing", () => {
  expect(queueSummary({ executing: 0, waiting: 2 })).toBe("No run is executing. 2 runs are waiting in line.");
});
