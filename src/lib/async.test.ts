import { describe, expect, it, vi } from "vitest";
import { waitForImagesOrTimeout, waitForPromiseOrTimeout } from "./async";

describe("waitForPromiseOrTimeout", () => {
  it("continues when the prerequisite settles", async () => {
    await expect(waitForPromiseOrTimeout(Promise.resolve(), 1_000)).resolves.toBeUndefined();
  });

  it("continues when the prerequisite stalls", async () => {
    vi.useFakeTimers();
    try {
      const waiting = waitForPromiseOrTimeout(new Promise(() => undefined), 25);
      await vi.advanceTimersByTimeAsync(25);
      await expect(waiting).resolves.toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("waitForImagesOrTimeout", () => {
  it("waits for dynamically inserted images to decode", async () => {
    const root = document.createElement("div");
    root.innerHTML = '<img src="data:image/png;base64,aW1hZ2U=">';
    const image = root.querySelector("img")!;
    let finishDecode!: () => void;
    const decode = vi.fn(
      () => new Promise<void>((resolve) => {
        finishDecode = resolve;
      }),
    );
    Object.defineProperty(image, "decode", { configurable: true, value: decode });
    let completed = false;

    const waiting = waitForImagesOrTimeout(root, 1_000).then(() => {
      completed = true;
    });
    await Promise.resolve();

    expect(decode).toHaveBeenCalledOnce();
    expect(completed).toBe(false);
    finishDecode();
    await waiting;
    expect(completed).toBe(true);
  });

  it("preloads Mermaid SVG images before export completes", async () => {
    const root = document.createElement("div");
    root.innerHTML = `
      <svg xmlns="http://www.w3.org/2000/svg">
        <image href="data:image/png;base64,aW1hZ2U=" />
      </svg>
    `;
    let finishDecode!: () => void;
    const decode = vi.fn(
      () => new Promise<void>((resolve) => {
        finishDecode = resolve;
      }),
    );
    const previousDecode = Object.getOwnPropertyDescriptor(
      HTMLImageElement.prototype,
      "decode",
    );
    Object.defineProperty(HTMLImageElement.prototype, "decode", {
      configurable: true,
      value: decode,
    });

    try {
      let completed = false;
      const waiting = waitForImagesOrTimeout(root, 1_000).then(() => {
        completed = true;
      });
      await Promise.resolve();

      expect(decode).toHaveBeenCalledOnce();
      expect(completed).toBe(false);
      finishDecode();
      await waiting;
      expect(completed).toBe(true);
    } finally {
      if (previousDecode) {
        Object.defineProperty(
          HTMLImageElement.prototype,
          "decode",
          previousDecode,
        );
      } else {
        delete (HTMLImageElement.prototype as { decode?: unknown }).decode;
      }
    }
  });

  it("continues when an image never finishes", async () => {
    vi.useFakeTimers();
    try {
      const root = document.createElement("div");
      root.innerHTML = '<img src="https://example.com/stalled.png">';
      const image = root.querySelector("img")!;
      Object.defineProperty(image, "decode", {
        configurable: true,
        value: () => new Promise<void>(() => undefined),
      });

      const waiting = waitForImagesOrTimeout(root, 25);
      await vi.advanceTimersByTimeAsync(25);

      await expect(waiting).resolves.toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });
});
