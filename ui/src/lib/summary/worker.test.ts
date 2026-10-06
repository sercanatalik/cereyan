// The worker status page's health headline, one sentence per status.
import { expect, test } from "vitest";
import { type WorkerHeadlineInput, workerHeadline } from "./worker";

const base: WorkerHeadlineInput = { status: "online", busy: 0, processors: 2, runnable: 19, drift: [] };

test("online and idle", () => {
  expect(workerHeadline(base)).toBe(
    "Healthy and idle. This worker picks up runs when the server's own processors are full, for any of the 19 flows whose code it has.",
  );
});

test("online and busy on some processors", () => {
  expect(workerHeadline({ ...base, busy: 1 })).toBe(
    "Healthy and busy. 1 of 2 processors is running a run; the rest pick up runs when the server's own processors are full, for any of the 19 flows whose code it has.",
  );
});

test("online and full", () => {
  expect(workerHeadline({ ...base, busy: 2 })).toMatch(
    /^Healthy and full\. All 2 processors are running a run/,
  );
  expect(workerHeadline({ ...base, busy: 1, processors: 1 })).toMatch(
    /^Healthy and full\. Its one processor is/,
  );
});

test("online with busy unknown", () => {
  expect(workerHeadline({ ...base, busy: null })).toMatch(/^Healthy\. This worker picks up runs/);
});

test("one flow reads in the singular", () => {
  expect(workerHeadline({ ...base, runnable: 1 })).toMatch(/any of the 1 flow whose/);
});

test("draining, busy and idle", () => {
  expect(workerHeadline({ ...base, status: "draining", busy: 2 })).toMatch(
    /^Draining\. 2 runs are finishing.*takes no new run\.$/,
  );
  expect(workerHeadline({ ...base, status: "draining", busy: 0 })).toMatch(/^Draining and idle\./);
});

test("older code names its modules", () => {
  const line = workerHeadline({ ...base, status: "drift", drift: ["reports"], runnable: 18 });
  expect(line).toMatch(/^Running older code in reports\. This worker takes no run of that module/);
  expect(line).toMatch(/18 flows/);
});

test("server unreachable, registering and refused", () => {
  expect(workerHeadline({ ...base, status: "unreachable" })).toMatch(/^Can't reach the server\./);
  expect(workerHeadline({ ...base, status: "registering" })).toMatch(/^Not registered yet\./);
  expect(workerHeadline({ ...base, status: "refused" })).toMatch(/^Refused by the server\./);
});
