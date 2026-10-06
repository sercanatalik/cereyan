import { expect, test } from "vitest";
import { ordinal, runSummary } from "./run";

const st = (type: any, name = type) => ({ type, name });

test("ordinals", () => {
  expect([1, 2, 3, 4, 11, 12, 13, 21, 22, 101].map(ordinal)).toEqual([
    "1st",
    "2nd",
    "3rd",
    "4th",
    "11th",
    "12th",
    "13th",
    "21st",
    "22nd",
    "101st",
  ]);
});

test("Completed", () => {
  expect(
    runSummary({ state: st("Completed"), duration: 26_000, attempt: 0, tasks: { completed: 3, total: 3 } }),
  ).toBe("Completed in 26 ms. 3 of 3 tasks completed.");
});

test("Failed on a later attempt", () => {
  expect(
    runSummary({ state: st("Failed"), duration: 41_000, attempt: 1, tasks: { completed: 2, total: 3 } }),
  ).toBe("Failed after 41 ms on the 2nd attempt. 2 of 3 tasks completed.");
});

test("Crashed and Cancelled", () => {
  expect(runSummary({ state: st("Crashed"), duration: 2_000_000, tasks: { completed: 0, total: 1 } })).toBe(
    "Crashed after 2.00 s. 0 of 1 task completed.",
  );
  expect(runSummary({ state: st("Cancelled"), duration: null, tasks: { completed: 0, total: 0 } })).toBe(
    "Cancelled.",
  );
});

test("Running and Cancelling show the time so far", () => {
  expect(
    runSummary({ state: st("Running"), duration: 12_300_000, attempt: 2, tasks: { completed: 1, total: 4 } }),
  ).toBe("Running for 12.30 s on the 3rd attempt. 1 of 4 tasks completed.");
  expect(runSummary({ state: st("Cancelling"), duration: 500_000, tasks: { completed: 0, total: 0 } })).toBe(
    "Cancelling after 500 ms.",
  );
});

test("Paused waits for input", () => {
  expect(runSummary({ state: st("Paused"), duration: 3_000, tasks: { completed: 1, total: 1 } })).toBe(
    "Waiting for input after 3 ms. 1 of 1 task completed.",
  );
});

test("Pending and Scheduled have not started", () => {
  expect(runSummary({ state: st("Pending"), tasks: { completed: 0, total: 0 } })).toBe(
    "Pending, waiting for an engine.",
  );
  expect(runSummary({ state: st("Scheduled"), tasks: { completed: 0, total: 0 } })).toBe(
    "Scheduled, not started yet.",
  );
  expect(runSummary({ state: st("Scheduled", "Late"), tasks: { completed: 0, total: 0 } })).toBe(
    "Late: past its scheduled time and not started.",
  );
  expect(
    runSummary({ state: st("Scheduled", "AwaitingRetry"), attempt: 1, tasks: { completed: 0, total: 0 } }),
  ).toBe("Waiting to retry on the 2nd attempt.");
});
