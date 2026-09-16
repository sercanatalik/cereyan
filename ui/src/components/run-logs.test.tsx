import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

// The API client captures globalThis.fetch when it loads, so the stub must exist first.
const fetchMock = vi.hoisted(() => {
  const fn = vi.fn();
  globalThis.fetch = fn as unknown as typeof fetch;
  const NativeRequest = globalThis.Request;
  globalThis.Request = class extends NativeRequest {
    constructor(input: RequestInfo | URL, init?: RequestInit) {
      super(typeof input === "string" && input.startsWith("/") ? `http://localhost${input}` : input, init);
    }
  } as typeof Request;
  return fn;
});

const live = vi.hoisted(() => ({ listeners: new Map<string, (data: any) => void>() }));
vi.mock("@/lib/live", () => ({
  useLiveEvent: (kind: string, callback: (data: any) => void) => live.listeners.set(kind, callback),
}));

import { RunLogs } from "./run-logs";

let logs: { id: number; timestamp: number; level: number; logger: string; message: string }[] = [];
const scrolled = vi.fn();

function line(id: number, message: string) {
  return { id, timestamp: 1_700_000_000_000_000 + id, level: 20, logger: "flow", message };
}

beforeEach(() => {
  logs = [line(1, "first")];
  scrolled.mockReset();
  Element.prototype.scrollIntoView = scrolled;
  fetchMock.mockImplementation(
    async () =>
      new Response(JSON.stringify({ items: logs, next_cursor: null }), {
        headers: { "content-type": "application/json" },
      }),
  );
});
afterEach(() => fetchMock.mockReset());

function mount() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <RunLogs runId={7} active />
    </QueryClientProvider>,
  );
}

async function append(message: string) {
  logs = [...logs, line(logs.length + 1, message)];
  act(() => live.listeners.get("log.appended")?.({ run_id: 7 }));
  await screen.findByText(message);
}

test("follow keeps the newest line in view as lines arrive", async () => {
  mount();
  await screen.findByText("first");
  await waitFor(() => expect(scrolled).toHaveBeenCalled());
  scrolled.mockClear();
  await append("second");
  await waitFor(() => expect(scrolled).toHaveBeenCalled());
});

test("turning follow off leaves the view where it is", async () => {
  mount();
  await screen.findByText("first");
  fireEvent.click(screen.getByRole("switch", { name: "Follow" }));
  scrolled.mockClear();
  await append("second");
  expect(scrolled).not.toHaveBeenCalled();
});
