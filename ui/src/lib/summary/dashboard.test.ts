import { expect, test } from "vitest";
import type { Flow, Run } from "@/api/client";
import {
  dashboardFlows,
  defaultPreviewRun,
  flowStatusLine,
  idleProcessors,
  upcomingReason,
} from "./dashboard";

type Tuple = Flow["recent_runs"][number];

const flow = (name: string, recent: Tuple[], extra: Partial<Flow> = {}) =>
  ({ name, project: "proj", recent_runs: recent, schedules: [], ...extra }) as unknown as Flow;

test("orders failing, then paused, then by latest run, and drops never-run flows", () => {
  const flows = [
    flow("report", [[40, "Completed", "Completed", 1]]),
    flow("old", [[10, "Completed", "Completed", 1]]),
    flow("flaky_load", [[30, "Failed", "Failed", 41_000]]),
    flow("crashy", [[5, "Crashed", "Crashed", null]]),
    flow("approve", [[35, "Paused", "Paused", null]]),
    flow("render", []),
  ];
  expect(dashboardFlows(flows).map((f) => f.name)).toEqual([
    "flaky_load",
    "crashy",
    "approve",
    "report",
    "old",
  ]);
});

test("keeps to the scope and to eight flows", () => {
  const flows = Array.from({ length: 10 }, (_, i) =>
    flow(`f${i}`, [[i + 1, "Completed", "Completed", 1]], { project: i < 9 ? "a" : "b" }),
  );
  expect(dashboardFlows(flows)).toHaveLength(8);
  expect(dashboardFlows(flows, "b").map((f) => f.name)).toEqual(["f9"]);
  expect(
    dashboardFlows([flow("g", [[1, "Completed", "Completed", 1]], { group: "x" })], "proj", "y"),
  ).toEqual([]);
});

test("the default preview run is the newest failed or crashed run of the listed flows", () => {
  const flows = [
    flow("a", [
      [9, "Completed", "Completed", 1],
      [7, "Failed", "Failed", 1],
    ]),
    flow("b", [[8, "Crashed", "Crashed", 1]]),
  ];
  expect(defaultPreviewRun(flows)).toBe(8);
  expect(defaultPreviewRun([flow("c", [[1, "Completed", "Completed", 1]])])).toBeNull();
});

test("the status line names the latest run, its duration and the next fire", () => {
  expect(flowStatusLine(flow("a", [[1, "Failed", "Failed", 41_000]]))).toEqual({
    text: "Failed · 41 ms",
    failing: true,
  });
  expect(flowStatusLine(flow("b", [[1, "Paused", "Paused", null]]))).toEqual({
    text: "Paused",
    failing: false,
  });
  const next = Date.now() * 1000 + 3_600_000_000;
  const scheduled = flow("c", [[1, "Completed", "Completed", 2_000_000]], {
    schedules: [
      { active: false, next_fire: next - 1 },
      { active: true, next_fire: next },
    ] as Flow["schedules"],
  });
  const line = flowStatusLine(scheduled);
  expect(line.text).toMatch(/^Completed · 2\.00 s · next \S+/);
  expect(line.failing).toBe(false);
});

test("an upcoming run is due to its schedule, a retry, a backfill, or a delayed start", () => {
  const r = (name: string, created_by: string) => ({ state: { name }, created_by }) as unknown as Run;
  expect(upcomingReason(r("Scheduled", "schedule:3"))).toBe("schedule");
  expect(upcomingReason(r("AwaitingRetry", "schedule:3"))).toBe("retry");
  expect(upcomingReason(r("Scheduled", "backfill:1"))).toBe("backfill");
  expect(upcomingReason(r("Scheduled", "ui"))).toBe("delayed start");
});

test("idle processors count free slots on hosts that are not offline", () => {
  const base = { cap: 8, load: null, source: "settings" };
  expect(
    idleProcessors({
      ...base,
      count: 4,
      items: [],
      hosts: [
        { host: "server", count: 4, busy: 1, state: "online" },
        { host: "w1", count: 2, busy: 0, state: "online" },
        { host: "w2", count: 2, busy: 0, state: "offline" },
      ],
    }),
  ).toBe(5);
  expect(
    idleProcessors({
      ...base,
      count: 4,
      hosts: [],
      items: [{ host: "server", id: "e1", module: "m", since_secs: 1, slot: 1, status: "running" }],
    }),
  ).toBe(3);
});
