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
  it("flushes writes appended while an earlier write is still pending", async () => {
    const writes: Array<ReturnType<typeof deferred<TestSettings>>> = [];
    const writer = createLatestSerializedWriter<TestSettings>(() => {
      const pending = deferred<TestSettings>();
      writes.push(pending);
      return pending.promise;
    }, () => undefined);
    await writer.flush();
    expect(writes).toHaveLength(0);
    const value = { theme: "dark", recentFiles: [] };
    const first = writer.enqueue(value);
    let flushed = false;
    const flushing = writer.flush().then(() => { flushed = true; });
    const second = writer.enqueue({ ...value, theme: "light" });
    await Promise.resolve();
    writes[0].resolve(value);
    await first;
    await Promise.resolve();
    expect(flushed).toBe(false);
    expect(writes).toHaveLength(2);
    writes[1].resolve({ ...value, theme: "light" });
    await second;
    await flushing;
    expect(flushed).toBe(true);
  });

  it("reports a failed latest write, but a newer successful snapshot supersedes it", async () => {
    const writes: Array<ReturnType<typeof deferred<TestSettings>>> = [];
    const writer = createLatestSerializedWriter<TestSettings>(() => {
      const pending = deferred<TestSettings>();
      writes.push(pending);
      return pending.promise;
    }, () => undefined);
    const value = { theme: "dark", recentFiles: [] };
    const first = writer.enqueue(value);
    const flushing = writer.flush();
    const second = writer.enqueue({ ...value, theme: "light" });
    await Promise.resolve();
    writes[0].reject(new Error("first failed"));
    await expect(first).rejects.toThrow("first failed");
    await Promise.resolve();
    writes[1].resolve(value);
    await second;
    await expect(flushing).resolves.toBeUndefined();

    const failed = writer.enqueue(value);
    await Promise.resolve();
    writes[2].reject(new Error("latest failed"));
    await expect(failed).rejects.toThrow("latest failed");
    await expect(writer.flush()).rejects.toThrow("latest failed");
  });

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
