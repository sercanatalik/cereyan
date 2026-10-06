/**
 * The worker status page's health headline: one sentence saying whether the
 * worker is healthy, busy or idle, and what it takes.
 */

/** What the headline reads: the page's status and what the worker knows. */
export type WorkerHeadlineInput = {
  /** The page's status: online, draining, drift, unreachable, registering or refused. */
  status: string;
  /** Processors running a run now; null when only the server could say. */
  busy: number | null;
  processors: number;
  /** Flows whose code this worker has and may run here. */
  runnable: number;
  /** Modules whose code differs from the server's. */
  drift: string[];
};

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

const takes = (runnable: number, verb = "picks") =>
  `${verb} up runs when the server's own processors are full, for any of the ${plural(runnable, "flow")} whose code it has`;

/** One sentence on the worker's health, per status and how busy it is. */
export function workerHeadline(w: WorkerHeadlineInput): string {
  const busy = w.busy ?? 0;
  switch (w.status) {
    case "unreachable":
      return "Can't reach the server. Its engines keep running and their runs report when the server answers again; it takes no new run until then.";
    case "registering":
      return "Not registered yet. This worker takes no run until the server accepts it.";
    case "refused":
      return "Refused by the server. This worker takes no run; its messages below say why.";
    case "draining":
      return busy > 0
        ? `Draining. ${plural(busy, "run")} ${busy === 1 ? "is" : "are"} finishing, then its processors stop; it takes no new run.`
        : "Draining and idle. This worker takes no new run; resume it from the server's Workers tab.";
    case "drift":
      return `Running older code in ${w.drift.join(", ")}. This worker takes no run of ${w.drift.length === 1 ? "that module" : "those modules"} until its checkout is updated, and still ${takes(w.runnable)}.`;
    default:
      if (w.busy === null) return `Healthy. This worker ${takes(w.runnable)}.`;
      if (busy === 0) return `Healthy and idle. This worker ${takes(w.runnable)}.`;
      if (busy >= w.processors)
        return `Healthy and full. ${w.processors === 1 ? "Its one processor is" : `All ${w.processors} processors are`} running a run; it takes the next one as soon as a run finishes.`;
      return `Healthy and busy. ${busy} of ${plural(w.processors, "processor")} ${busy === 1 ? "is" : "are"} running a run; the rest ${takes(w.runnable, "pick")}.`;
  }
}
