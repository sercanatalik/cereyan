// The dashboard's queue-depth sparkline draws the history samples and labels the latest value.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { QueueSparkline } from "./queue-sparkline";

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

beforeEach(() => {
  fetchMock.mockImplementation(
    async () =>
      new Response(
        JSON.stringify({
          interval_secs: 5,
          samples: [
            { at: 1, queued: 0, running: 0, engines_busy: 0 },
            { at: 2, queued: 4, running: 1, engines_busy: 1 },
            { at: 3, queued: 2, running: 2, engines_busy: 2 },
          ],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
  );
});

test("draws one point per sample and shows the latest queue depth", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <QueueSparkline width={100} height={20} />
    </QueryClientProvider>,
  );
  const box = await screen.findByTestId("queue-sparkline");
  await waitFor(() => expect(box.textContent).toContain("2"));
  const polyline = box.querySelector("polyline");
  expect(polyline?.getAttribute("points")?.split(" ")).toHaveLength(3);
  expect(box.querySelector("svg")?.getAttribute("aria-label")).toBe("queue depth over 3 samples");
});
