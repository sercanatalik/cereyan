// Live updates over server-sent events: one connection, sequence tracking,
// query-cache patching for runs and task runs, invalidation for lists, and
// a resync when the server's ring buffer no longer covers our position.
import { type QueryClient, useQueryClient } from "@tanstack/react-query";
import { createContext, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { Run, TaskRun } from "@/api/client";

export type LiveStatus = "connecting" | "live" | "reconnecting";

export interface LiveEvent {
  kind: string;
  seq: number;
  data: any;
}

interface LiveContextValue {
  status: LiveStatus;
  lastSeq: number;
  subscribe: (listener: (event: LiveEvent) => void) => () => void;
}

const LiveContext = createContext<LiveContextValue>({
  status: "connecting",
  lastSeq: 0,
  subscribe: () => () => {},
});

export function applyEvent(client: QueryClient, event: LiveEvent) {
  switch (event.kind) {
    case "run.updated": {
      const run = event.data as Run & { deleted?: boolean };
      if (run.deleted) {
        client.removeQueries({ queryKey: ["run", run.id] });
      } else {
        client.setQueryData(["run", run.id], run);
        client.setQueriesData({ queryKey: ["runs"] }, (old: any) => patchList(old, run));
      }
      client.invalidateQueries({ queryKey: ["counts"] });
      client.invalidateQueries({ queryKey: ["runs"], refetchType: "none" });
      client.invalidateQueries({ queryKey: ["flows"] });
      break;
    }
    case "task_run.updated": {
      const taskRun = event.data as TaskRun;
      client.setQueryData(["task-run", taskRun.id], taskRun);
      client.setQueriesData({ queryKey: ["task-runs"] }, (old: any) => patchList(old, taskRun));
      client.setQueryData(["run-tasks", taskRun.run_id], (old: TaskRun[] | undefined) => {
        if (!old) return old;
        const index = old.findIndex((t) => t.id === taskRun.id);
        if (index < 0) return [...old, taskRun];
        const next = old.slice();
        next[index] = taskRun;
        return next;
      });
      client.invalidateQueries({ queryKey: ["run-tasks", taskRun.run_id], refetchType: "none" });
      client.invalidateQueries({ queryKey: ["counts"] });
      break;
    }
    case "log.appended":
      client.invalidateQueries({ queryKey: ["logs", event.data.run_id] });
      break;
    case "flow.registered":
      client.invalidateQueries({ queryKey: ["flows"] });
      break;
    case "rule.updated":
      client.invalidateQueries({ queryKey: ["rules"] });
      client.invalidateQueries({ queryKey: ["firings"] });
      break;
    case "variable.updated":
      client.invalidateQueries({ queryKey: ["variables"] });
      break;
    case "schedule.updated":
    case "backfill.created":
    case "backfill.updated":
      client.invalidateQueries({ queryKey: ["flows"] });
      client.invalidateQueries({ queryKey: ["upcoming"] });
      break;
    case "resync":
      client.invalidateQueries();
      break;
    default:
      break;
  }
}

function patchList(page: any, item: { id: number }) {
  if (!page || !Array.isArray(page.items)) return page;
  const index = page.items.findIndex((r: { id: number }) => r.id === item.id);
  if (index < 0) return page;
  const items = page.items.slice();
  items[index] = item;
  return { ...page, items };
}

export function LiveProvider({ children }: { children: React.ReactNode }) {
  const client = useQueryClient();
  const [status, setStatus] = useState<LiveStatus>("connecting");
  const lastSeq = useRef(0);
  const listeners = useRef(new Set<(event: LiveEvent) => void>());

  useEffect(() => {
    let source: EventSource | null = null;
    let closed = false;
    let retry = 500;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const connect = () => {
      if (closed) return;
      const since = lastSeq.current ? `?since=${lastSeq.current}` : "";
      source = new EventSource(`/api/stream${since}`);
      const handle = (kind: string) => (raw: MessageEvent) => {
        const seq = Number(raw.lastEventId || 0);
        if (seq) lastSeq.current = seq;
        let data: any = null;
        try {
          data = raw.data ? JSON.parse(raw.data) : null;
        } catch {
          data = null;
        }
        const event: LiveEvent = { kind, seq, data };
        applyEvent(client, event);
        for (const listener of listeners.current) listener(event);
      };
      for (const kind of [
        "run.updated",
        "task_run.updated",
        "log.appended",
        "flow.registered",
        "event.created",
        "rule.updated",
        "variable.updated",
        "artifact.updated",
        "schedule.updated",
        "backfill.created",
        "backfill.updated",
        "resync",
      ]) {
        source.addEventListener(kind, handle(kind));
      }
      source.addEventListener("hello", (raw: MessageEvent) => {
        try {
          const hello = JSON.parse(raw.data);
          if (!lastSeq.current && typeof hello.latest === "number") lastSeq.current = hello.latest;
        } catch {}
        setStatus("live");
        retry = 500;
      });
      source.onopen = () => setStatus("live");
      source.onerror = () => {
        setStatus("reconnecting");
        source?.close();
        timer = setTimeout(connect, retry);
        retry = Math.min(retry * 2, 10_000);
      };
    };
    connect();
    return () => {
      closed = true;
      if (timer) clearTimeout(timer);
      source?.close();
    };
  }, [client]);

  const value = useMemo<LiveContextValue>(
    () => ({
      status,
      lastSeq: lastSeq.current,
      subscribe: (listener) => {
        listeners.current.add(listener);
        return () => listeners.current.delete(listener);
      },
    }),
    [status],
  );
  return <LiveContext.Provider value={value}>{children}</LiveContext.Provider>;
}

export function useLiveUpdates(): LiveContextValue {
  return useContext(LiveContext);
}

/** Subscribe to events of one kind; the callback receives the parsed data. */
export function useLiveEvent(kind: string, callback: (data: any) => void) {
  const { subscribe } = useLiveUpdates();
  const ref = useRef(callback);
  ref.current = callback;
  useEffect(() => subscribe((event) => event.kind === kind && ref.current(event.data)), [subscribe, kind]);
}
