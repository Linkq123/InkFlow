import { afterEach, describe, expect, it, vi } from "vitest";

interface WorkerRequest {
  revision: number;
  markdown: string;
  operation: "render" | "detectRemoteImages";
}

class FakeWorker {
  static latest: FakeWorker | null = null;
  static instances: FakeWorker[] = [];

  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  requests: WorkerRequest[] = [];
  terminated = false;

  constructor() {
    FakeWorker.latest = this;
    FakeWorker.instances.push(this);
  }

  postMessage(request: WorkerRequest): void {
    this.requests.push(request);
  }

  terminate(): void {
    this.terminated = true;
  }

  respond(data: Record<string, unknown>): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}

async function loadService() {
  vi.resetModules();
  vi.stubGlobal("Worker", FakeWorker);
  return import("./render-service");
}

afterEach(() => {
  vi.unstubAllGlobals();
  FakeWorker.latest = null;
  FakeWorker.instances = [];
});

describe("Markdown worker service", () => {
  it("keeps rendering and remote-image detection independent", async () => {
    const { detectRemoteImagesInWorker, renderInWorker } = await loadService();

    const render = renderInWorker("# Draft");
    const detection = detectRemoteImagesInWorker("![a](https://example.com/a.png)");
    const worker = FakeWorker.latest;

    expect(worker?.requests.map(({ operation }) => operation))
      .toEqual(["render", "detectRemoteImages"]);
    worker?.respond({ revision: 2, hasRemoteImages: true });
    worker?.respond({ revision: 1, html: "<h1>Draft</h1>" });

    await expect(render).resolves.toBe("<h1>Draft</h1>");
    await expect(detection).resolves.toBe(true);
  });

  it("supersedes only an older request of the same operation", async () => {
    const { detectRemoteImagesInWorker } = await loadService();

    const first = detectRemoteImagesInWorker("first");
    const firstOutcome = first.catch((error: unknown) => error);
    const second = detectRemoteImagesInWorker("second");
    const firstWorker = FakeWorker.instances[0];
    const replacement = FakeWorker.latest;
    const secondRevision = replacement?.requests.at(-1)?.revision;

    await expect(firstOutcome).resolves.toMatchObject({
      name: "AbortError",
    });
    expect(firstWorker.terminated).toBe(true);
    expect(replacement).not.toBe(firstWorker);
    replacement?.respond({ revision: secondRevision, hasRemoteImages: false });
    await expect(second).resolves.toBe(false);
  });

  it("replays a still-current operation after cancelling another one", async () => {
    const { detectRemoteImagesInWorker, renderInWorker } = await loadService();

    const render = renderInWorker("# Draft");
    const firstDetection = detectRemoteImagesInWorker("first");
    const firstOutcome = firstDetection.catch((error: unknown) => error);
    const secondDetection = detectRemoteImagesInWorker("second");
    const replacement = FakeWorker.latest;

    await expect(firstOutcome).resolves.toMatchObject({ name: "AbortError" });
    expect(replacement?.requests.map(({ markdown, operation }) => ({ markdown, operation })))
      .toEqual([
        { markdown: "# Draft", operation: "render" },
        { markdown: "second", operation: "detectRemoteImages" },
      ]);

    const renderRevision = replacement?.requests[0]?.revision;
    const detectionRevision = replacement?.requests[1]?.revision;
    replacement?.respond({ revision: detectionRevision, hasRemoteImages: false });
    replacement?.respond({ revision: renderRevision, html: "<h1>Draft</h1>" });
    await expect(render).resolves.toBe("<h1>Draft</h1>");
    await expect(secondDetection).resolves.toBe(false);
  });
});
