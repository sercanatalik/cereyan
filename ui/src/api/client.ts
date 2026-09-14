import createClient from "openapi-fetch";
import { announceAuthRequired } from "@/components/token-prompt";
import { basePath } from "@/lib/base";
import type { components, paths } from "./schema";

export const api = createClient<paths>({ baseUrl: basePath() });

// A 401 means the server wants a token: raise the prompt (see TokenPrompt).
api.use({
  onResponse({ request, response }) {
    if (response.status === 401) {
      const hadCookie = document.cookie.split(";").some((c) => c.trim().startsWith("cereyan_token="));
      announceAuthRequired(hadCookie || request.headers.has("authorization"));
    }
    return response;
  },
});

export type Run = components["schemas"]["Run"];
export type TaskRun = components["schemas"]["TaskRun"];
export type Flow = components["schemas"]["FlowSummary"];
export type Log = components["schemas"]["Log"];
export type RunState = components["schemas"]["State"];
export type StateType = components["schemas"]["StateType"];
export type Counts = components["schemas"]["Counts"];
export type RunsPage = components["schemas"]["RunsPage"];
export type TaskRunsPage = components["schemas"]["TaskRunsPage"];
export type LogsPage = components["schemas"]["LogsPage"];
export type ScheduleRow = components["schemas"]["ScheduleRow"];
export type UpcomingItem = components["schemas"]["UpcomingItem"];
export type UpcomingRun = components["schemas"]["UpcomingRun"];

/** A materialized run, as opposed to a fire projected past the look-ahead. */
export function isUpcomingRun(item: UpcomingItem): item is UpcomingRun {
  return "id" in item;
}

export class ApiError extends Error {
  status: number;
  body: unknown;
  constructor(status: number, body: unknown) {
    const message =
      typeof body === "object" && body && "error" in body
        ? String((body as { error: unknown }).error)
        : `HTTP ${status}`;
    super(message);
    this.status = status;
    this.body = body;
  }
}

export function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
  if (result.error !== undefined || !result.response.ok) {
    throw new ApiError(result.response.status, result.error);
  }
  return result.data as T;
}
