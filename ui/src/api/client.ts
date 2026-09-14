import createClient from "openapi-fetch";
import { announceSignInRequired } from "@/components/sign-in-required";
import { announceAuthRequired } from "@/components/token-prompt";
import { basePath } from "@/lib/base";
import type { components, paths } from "./schema";

export const api = createClient<paths>({ baseUrl: basePath() });

/**
 * A 401 names how the server authenticates: `hook` raises the not-signed-in
 * panel (see SignInRequired), anything else the token prompt (see TokenPrompt).
 */
export async function handleUnauthorized(request: Request, response: Response) {
  const body = await response
    .clone()
    .json()
    .catch(() => null);
  if (body?.auth === "hook") {
    announceSignInRequired(typeof body.login_url === "string" ? body.login_url : null);
    return;
  }
  const hadCookie = document.cookie.split(";").some((c) => c.trim().startsWith("cereyan_token="));
  announceAuthRequired(hadCookie || request.headers.has("authorization"));
}

api.use({
  async onResponse({ request, response }) {
    if (response.status === 401) {
      await handleUnauthorized(request, response);
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
