// The schedule editor carries the catch-up window, jitter, and start deadline both ways.
import { expect, test } from "vitest";
import { draftToBody, rowToDraft } from "./schedule-editor";

const row = {
  id: 1,
  external_id: "x",
  flow_id: 2,
  schedule: { kind: "interval", interval: 300, anchor: 0, timezone: "UTC" },
  catchup: "skip",
  catchup_max: 100,
  catchup_window: 3600,
  jitter: 30,
  start_deadline: 600,
  active: true,
  paused_reason: null,
  paused_until: null,
  source: "ui",
  code_key: null,
  persist: true,
  created_at: 0,
  updated_at: 0,
  next_fire: null,
  skipped: 0,
} as any;

test("a row's policies reach the draft and the request body", () => {
  const draft = rowToDraft(row);
  expect(draft).toMatchObject({ kind: "interval", catchup_window: 3600, jitter: 30, start_deadline: 600 });
  expect(draftToBody(draft)).toMatchObject({
    kind: "interval",
    interval: 300,
    catchup_window: 3600,
    jitter: 30,
    start_deadline: 600,
  });
});

test("off is null in the draft and zero in the body", () => {
  const draft = rowToDraft({ ...row, catchup_window: null, jitter: 0, start_deadline: null });
  expect(draft).toMatchObject({ catchup_window: null, jitter: 0, start_deadline: null });
  expect(draftToBody(draft)).toMatchObject({ catchup_window: 0, jitter: 0, start_deadline: 0 });
});
