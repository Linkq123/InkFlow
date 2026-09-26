import { describe, expect, it, vi } from "vitest";
import { createSettingsWriter } from "./settings-writer";

const defaults = { theme: "system", fontSize: 16, sidebar: false };
type Settings = typeof defaults;

describe("settings writer", () => {
  it("uses the submitted UI baseline for queued edits, then adopts the latest server baseline", async () => {
    let finishFirst!: (value: Settings) => void;
    const write = vi.fn(async (value: Settings, _baseline: Settings) => ({ ...value, fontSize: 20 }))
      .mockImplementationOnce(() => new Promise(resolve => finishFirst = resolve));
    const apply = vi.fn();
    const writer = createSettingsWriter(defaults, write, apply);
    const dark = { ...defaults, theme: "dark" };
    const first = writer.enqueue(dark);
    // Both snapshots precede the server response. A deliberate toggle back to
    // the original theme must survive, without echoing a stale font size.
    const next = { ...defaults, sidebar: true };
    const second = writer.enqueue(next);
    await Promise.resolve();
    finishFirst({ ...dark, fontSize: 20 });
    await first;
    await second;

    expect(write.mock.calls).toEqual([[dark, defaults], [next, dark]]);
    expect(apply).toHaveBeenCalledExactlyOnceWith({ ...next, fontSize: 20 });
    const third = { ...next, fontSize: 20, sidebar: false };
    await writer.enqueue(third);
    expect(write).toHaveBeenLastCalledWith(third, { ...next, fontSize: 20 });
  });

  it("retries intended fields after a failed request instead of advancing its baseline", async () => {
    const write = vi.fn(async (value: Settings, _baseline: Settings) => value)
      .mockRejectedValueOnce(new Error("disk unavailable"));
    const writer = createSettingsWriter(defaults, write, () => undefined);
    const first = writer.enqueue({ ...defaults, theme: "dark" });
    const next = { ...defaults, theme: "dark", sidebar: true };
    const second = writer.enqueue(next);
    await expect(first).rejects.toThrow("disk unavailable");
    await second;
    expect(write).toHaveBeenLastCalledWith(next, defaults);
  });

  it("compares early hydration edits against the loaded settings", async () => {
    const write = vi.fn(async (value: Settings, _baseline: Settings) => value);
    const writer = createSettingsWriter(defaults, write, () => undefined);
    const loaded = { ...defaults, theme: "light", fontSize: 24 };
    writer.initialize(loaded);
    const edited = { ...loaded, sidebar: true };
    await writer.enqueue(edited);
    expect(write).toHaveBeenCalledExactlyOnceWith(edited, loaded);
  });
});
