import type { StateType } from "@/api/client";
import { formatDuration } from "@/lib/utils";

export interface RunSummaryInput {
  state: { type: StateType; name: string };
  /** Run time so far in microseconds: the total once finished, elapsed while live. */
  duration?: number | null;
  /** The run's attempt, counted from 0 as the server stores it. */
  attempt?: number | null;
  /** Task runs of the pass on show, and how many of them completed. */
  tasks: { completed: number; total: number };
}

/** 1st, 2nd, 3rd, 4th … 11th, 12th, 13th … 21st. */
export function ordinal(n: number): string {
  const tens = n % 100;
  if (tens >= 11 && tens <= 13) return `${n}th`;
  const suffix = { 1: "st", 2: "nd", 3: "rd" }[n % 10] ?? "th";
  return `${n}${suffix}`;
}

/** The first sentence: what state the run is in and for how long. */
function stateClause({ state, duration, attempt }: RunSummaryInput): string {
  const time = duration != null ? formatDuration(duration) : null;
  const onAttempt = attempt && attempt > 0 ? ` on the ${ordinal(attempt + 1)} attempt` : "";
  switch (state.type) {
    case "Completed":
      return time ? `Completed in ${time}${onAttempt}` : `Completed${onAttempt}`;
    case "Failed":
    case "Crashed":
    case "Cancelled":
      return time ? `${state.type} after ${time}${onAttempt}` : `${state.type}${onAttempt}`;
    case "Cancelling":
      return time ? `Cancelling after ${time}` : "Cancelling";
    case "Running":
      return time ? `Running for ${time}${onAttempt}` : `Running${onAttempt}`;
    case "Paused":
      return time ? `Waiting for input after ${time}` : "Waiting for input";
    case "Pending":
      return `Pending, waiting for an engine${onAttempt}`;
    case "Scheduled":
      if (state.name === "Late") return "Late: past its scheduled time and not started";
      if (state.name === "AwaitingRetry") return `Waiting to retry${onAttempt}`;
      return `Scheduled, not started yet${onAttempt}`;
  }
}

/**
 * One sentence about a run for the top of its page, from its state, duration,
 * attempt and task counts: "Failed after 41 ms on the 2nd attempt. 2 of 3
 * tasks completed."
 */
export function runSummary(input: RunSummaryInput): string {
  const { completed, total } = input.tasks;
  const tasks = total > 0 ? ` ${completed} of ${total} ${total === 1 ? "task" : "tasks"} completed.` : "";
  return `${stateClause(input)}.${tasks}`;
}
