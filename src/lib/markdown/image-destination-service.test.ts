import { afterEach, describe, expect, it, vi } from "vitest";
import { cooperativeWork } from "../async";
import { collectImageDestinations, collectImageDestinationsAsync } from "./image-destinations";
import fixtures from "../../../tests/fixtures/image-rewrites.json";

class FakeWorker {
  static latest: FakeWorker;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  requests: Array<{ id: number; markdown: string }> = [];
  terminated = false;
  constructor() { FakeWorker.latest = this; }
  postMessage(request: { id: number; markdown: string }): void { this.requests.push(request); }
  terminate(): void { this.terminated = true; }
  respond(index: number): void {
    const { id, markdown } = this.requests[index];
    this.onmessage?.({ data: { id, images: collectImageDestinations(markdown) } } as MessageEvent);
  }
}

async function loadService(worker: unknown = FakeWorker) {
  vi.resetModules();
  vi.stubGlobal("Worker", worker);
  return import("./image-destination-service");
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("history image parsing", () => {
  it("keeps concurrent tab requests independent and accepts out-of-order responses", async () => {
    const { parseImageDestinations } = await loadService();
    const first = parseImageDestinations("![a](first.png)", cooperativeWork());
    const second = parseImageDestinations("![b](second.png)", cooperativeWork());
    FakeWorker.latest.respond(1);
    FakeWorker.latest.respond(0);
    await expect(first).resolves.toMatchObject([{ destination: "first.png" }]);
    await expect(second).resolves.toMatchObject([{ destination: "second.png" }]);
    expect(FakeWorker.latest.terminated).toBe(false);
  });

  it.each(["construction", "post", "error", "message", "timeout"])("recovers pending requests after worker %s failure", async failure => {
    class UnavailableWorker { constructor() { throw new Error("Unavailable"); } }
    const { parseImageDestinations } = await loadService(failure === "construction" ? UnavailableWorker : FakeWorker);
    if (failure === "post") vi.spyOn(FakeWorker.prototype, "postMessage").mockImplementation(() => { throw new Error("Post failed"); });
    if (failure === "timeout") vi.useFakeTimers();
    const first = parseImageDestinations("![a](first.png)", cooperativeWork());
    const second = parseImageDestinations("![b](second.png)", cooperativeWork());
    if (failure === "error") FakeWorker.latest.onerror?.({ preventDefault() {} } as ErrorEvent);
    if (failure === "message") FakeWorker.latest.onmessageerror?.();
    if (failure === "timeout") await vi.runAllTimersAsync();
    await expect(first).resolves.toMatchObject([{ destination: "first.png" }]);
    await expect(second).resolves.toMatchObject([{ destination: "second.png" }]);
  });

  it("matches synchronous parsing for the shared image fixtures", async () => {
    for (const fixture of fixtures) {
      await expect(collectImageDestinationsAsync(fixture.content)).resolves.toEqual(collectImageDestinations(fixture.content));
    }
  });

  it("yields to browser tasks during large-document fallback parsing", async () => {
    const { parseImageDestinations } = await loadService();
    vi.stubGlobal("Worker", undefined);
    const content = "![a](first.png)\n\n" + "ordinary paragraph text\n\n".repeat(40000);
    let heartbeats = 0;
    const timer = setInterval(() => heartbeats++, 0);
    try {
      await expect(parseImageDestinations(content, cooperativeWork())).resolves.toMatchObject([{ destination: "first.png" }]);
      expect(heartbeats).toBeGreaterThan(1);
    } finally { clearInterval(timer); }
  });
});
