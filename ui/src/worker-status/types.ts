/**
 * What a worker's status listener serves at `status.json`. Written by hand:
 * the worker is not part of the server's OpenAPI document. Keep in step with
 * `Worker.status()` in python/cereyan/worker.py.
 */

/** Runs of one flow that started on the worker since it started (from the server). */
export type FlowStats = {
  flow: string;
  completed: number;
  failed: number;
  crashed: number;
  cancelled: number;
  running: number;
  last_completed_at: number | null;
};

/** One engine and the run it executes (from the server). */
export type EngineStats = {
  engine_id: string;
  slot: number;
  module: string;
  status: string;
  run_id: number | null;
  flow: string | null;
  since_secs: number;
};

export type WorkerEvent = {
  /** Microseconds. */
  at: number;
  level: "info" | "warning" | "error";
  message: string;
  /** Consecutive identical messages kept as one. */
  count: number;
};

export type WorkerStatusPayload = {
  name: string;
  /** The server's URL, as given to --host. */
  server: string;
  /** The server's Workers tab. */
  server_ui: string;
  worker_id: number | null;
  /** starting, registering, online, draining, or refused. */
  state: string;
  server_reachable: boolean;
  heartbeat_secs: number;
  /** Microseconds; null before the first successful heartbeat or registration. */
  last_ok_heartbeat_at: number | null;
  failing_since: number | null;
  started_at: number;
  now: number;
  processors: number;
  cpus: number;
  version: string;
  drift: string[];
  refused: string[];
  /** Flows this checkout defines. */
  flows: { project: string; flow: string; module: string; runs_on?: string }[];
  /** Engine ids running on this machine now. */
  engines: string[];
  stats: { since: number; by_flow: FlowStats[]; engines: EngineStats[] } | null;
  stats_as_of: number | null;
  events: WorkerEvent[];
  host: {
    meta: { [key: string]: unknown };
    cpus: number;
    version: string;
    labels: { [key: string]: string };
    shared_paths: string[];
  };
};
