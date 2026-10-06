// The Flows page's quick filters, attention count and health words.
import { expect, test } from "vitest";
import type { Flow } from "@/api/client";
import { flowsAttention, healthLine, matchesQuickFilter, QUICK_FILTERS } from "./flows";

const flow = (latest: string | null, extra: Partial<Flow> = {}) =>
  ({
    recent_runs: latest
      ? [
          [2, latest, latest, 1_000],
          [1, "Completed", "Completed", 1_000],
        ]
      : [],
    schedules: [],
    ...extra,
  }) as unknown as Flow;

test("the chips are All, Failing, Scheduled, Waiting for input and Never run", () => {
  expect(QUICK_FILTERS.map((q) => q.label)).toEqual([
    "All",
    "Failing",
    "Scheduled",
    "Waiting for input",
    "Never run",
  ]);
});

test("Failing takes a latest run that Failed or Crashed, not an older one", () => {
  expect(matchesQuickFilter(flow("Failed"), "failing")).toBe(true);
  expect(matchesQuickFilter(flow("Crashed"), "failing")).toBe(true);
  expect(matchesQuickFilter(flow("Completed"), "failing")).toBe(false);
  const recovered = flow(null, {
    recent_runs: [
      [3, "Completed", "Completed", 1],
      [2, "Failed", "Failed", 1],
    ],
  } as Partial<Flow>);
  expect(matchesQuickFilter(recovered, "failing")).toBe(false);
});

test("Scheduled, Waiting for input, Never run and All", () => {
  expect(matchesQuickFilter(flow("Completed", { schedules: [{}] } as Partial<Flow>), "scheduled")).toBe(true);
  expect(matchesQuickFilter(flow("Completed"), "scheduled")).toBe(false);
  expect(matchesQuickFilter(flow("Paused"), "waiting")).toBe(true);
  expect(matchesQuickFilter(flow("Running"), "waiting")).toBe(false);
  expect(matchesQuickFilter(flow(null), "never")).toBe(true);
  expect(matchesQuickFilter(flow("Completed"), "never")).toBe(false);
  expect(matchesQuickFilter(flow(null), "all")).toBe(true);
});

test("the attention count names failing and waiting flows, and is quiet without them", () => {
  expect(flowsAttention([flow("Completed"), flow(null)])).toBeNull();
  expect(flowsAttention([flow("Failed")])).toBe("1 is failing");
  expect(flowsAttention([flow("Failed"), flow("Crashed"), flow("Paused")])).toBe(
    "2 are failing, 1 is waiting for input",
  );
  expect(flowsAttention([flow("Paused"), flow("Paused")])).toBe("2 are waiting for input");
});

test("health in words, failing marked as a failure", () => {
  expect(healthLine(null)).toBeNull();
  expect(healthLine({ status: "PASS", reasons: [] })).toEqual({
    text: "Health check passing",
    failing: false,
  });
  expect(healthLine({ status: "WARN", reasons: ["deadline in 10 minutes"] })).toEqual({
    text: "Health check warning: deadline in 10 minutes",
    failing: false,
  });
  expect(healthLine({ status: "FAIL", reasons: ["target is 3 hours old", "missed 06:00"] })).toEqual({
    text: "Health check failing: target is 3 hours old; missed 06:00",
    failing: true,
  });
});
