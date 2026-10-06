/**
 * The Queue page's opening line: how many runs are executing and how many
 * wait in line for a processor.
 */

export type QueueSummaryInput = {
  /** Runs a processor is executing now, on the server or a worker. */
  executing: number;
  /** Runs in line, listed or not. */
  waiting: number;
};

const runs = (n: number) => `${n} ${n === 1 ? "run" : "runs"}`;
const is = (n: number) => (n === 1 ? "is" : "are");

/** One sentence on what the processors are doing and what waits for them. */
export function queueSummary({ executing, waiting }: QueueSummaryInput): string {
  const first = executing
    ? `${runs(executing)} ${is(executing)} executing.`
    : waiting
      ? "No run is executing."
      : "No run is executing or waiting.";
  if (!waiting) return executing ? `${first} Nothing is waiting, so new runs start at once.` : first;
  return `${first} ${runs(waiting)} ${is(waiting)} waiting in line.`;
}
