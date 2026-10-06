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

function at(id: number, level: number, message: string, micros = 1_700_000_000_000_000 + id) {
  return { id, timestamp: micros, level, logger: "flow", message };
}

test("level chips count each level and filter to one", async () => {
  logs = [
    ...Array.from({ length: 11 }, (_, i) => at(i + 1, 20, `info ${i + 1}`)),
    at(12, 30, "slow"),
    at(13, 40, "bad row"),
    at(14, 50, "gave up"),
  ];
  mount();
  await screen.findByText("gave up");
  const chips = screen.getAllByTestId("level-chip").map((c) => c.textContent);
  expect(chips).toEqual(["All14", "Info11", "Warning1", "Error2"]);
  fireEvent.click(screen.getByRole("button", { name: /^Error/ }));
  expect(screen.getByRole("button", { name: /^Error/ })).toHaveAttribute("aria-pressed", "true");
  expect(screen.queryByText("slow")).toBeNull();
  expect(screen.getByText("bad row")).toBeInTheDocument();
  expect(screen.getByText("gave up")).toBeInTheDocument();
  // Counts stay those of the whole log.
  expect(screen.getByRole("button", { name: /^All/ })).toHaveTextContent("All14");
});

test("lines show the time only, under a header for each day", async () => {
  const day1 = new Date(2026, 9, 5, 20, 27, 19, 31).getTime() * 1000;
  const day2 = new Date(2026, 9, 6, 1, 2, 3, 4).getTime() * 1000;
  logs = [at(1, 20, "late", day1), at(2, 20, "early", day2)];
  mount();
  await screen.findByText("early");
  const headers = screen.getAllByTestId("log-date");
  expect(headers).toHaveLength(2);
  expect(headers[0]).toHaveTextContent(/Times are local · .*2026/);
  expect(screen.getByText("20:27:19.031")).toBeInTheDocument();
  expect(screen.getByText("01:02:03.004")).toBeInTheDocument();
});

test("download saves the shown lines as text", async () => {
  logs = [at(1, 20, "first"), at(2, 40, "boom")];
  const blobs: Blob[] = [];
  URL.createObjectURL = vi.fn((b: Blob) => {
    blobs.push(b);
    return "blob:logs";
  }) as any;
  URL.revokeObjectURL = vi.fn();
  const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
  mount();
  await screen.findByText("boom");
  fireEvent.click(screen.getByRole("button", { name: "Download" }));
  expect(click).toHaveBeenCalled();
  // jsdom's Blob has no text(); read it the old way.
  const text = await new Promise<string>((done) => {
    const reader = new FileReader();
    reader.onload = () => done(reader.result as string);
    reader.readAsText(blobs[0]);
  });
  expect(text).toMatch(/INFO +flow first\n.*ERROR +flow boom\n$/);
  click.mockRestore();
});
