import { describe, expect, it, vi } from "vitest";
import { waitForPromiseOrTimeout } from "./async";

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
