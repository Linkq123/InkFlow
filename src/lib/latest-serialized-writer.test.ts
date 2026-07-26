import { describe, expect, it } from "vitest";
import { createLatestSerializedWriter } from "./latest-serialized-writer";

interface TestSettings {
  theme: string;
  recentFiles: string[];
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("latest serialized writer", () => {
  it("serializes writes and applies only the newest response", async () => {
    const pending: Array<ReturnType<typeof deferred<TestSettings>>> = [];
    const written: TestSettings[] = [];
    const applied: TestSettings[] = [];
    const writer = createLatestSerializedWriter<TestSettings>(
      (value) => {
        written.push(value);
        const operation = deferred<TestSettings>();
        pending.push(operation);
        return operation.promise;
      },
      (value) => applied.push(value),
    );

    const firstValue = { theme: "light", recentFiles: [] };
    const first = writer.enqueue(firstValue);
    firstValue.theme = "mutated-after-enqueue";
    const second = writer.enqueue({ theme: "dark", recentFiles: ["note.md"] });
    await Promise.resolve();

    expect(written).toEqual([{ theme: "light", recentFiles: [] }]);
    pending[0].resolve({ theme: "light-normalized", recentFiles: [] });
    await first;
    await Promise.resolve();

    expect(applied).toEqual([]);
    expect(written).toEqual([
      { theme: "light", recentFiles: [] },
      { theme: "dark", recentFiles: ["note.md"] },
    ]);

    pending[1].resolve({ theme: "dark-normalized", recentFiles: ["note.md"] });
    await second;
    await Promise.resolve();

    expect(applied).toEqual([
      { theme: "dark-normalized", recentFiles: ["note.md"] },
    ]);
  });

  it("continues with newer writes after an earlier failure", async () => {
    const pending: Array<ReturnType<typeof deferred<TestSettings>>> = [];
    const writer = createLatestSerializedWriter<TestSettings>(
      () => {
        const operation = deferred<TestSettings>();
        pending.push(operation);
        return operation.promise;
      },
      () => undefined,
    );

    const first = writer.enqueue({ theme: "light", recentFiles: [] });
    const second = writer.enqueue({ theme: "dark", recentFiles: [] });
    await Promise.resolve();
    pending[0].reject(new Error("disk unavailable"));
    await expect(first).rejects.toThrow("disk unavailable");
    await Promise.resolve();

    expect(pending).toHaveLength(2);
    pending[1].resolve({ theme: "dark", recentFiles: [] });
    await expect(second).resolves.toEqual({ theme: "dark", recentFiles: [] });
  });
});
