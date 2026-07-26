import { describe, expect, it } from "vitest";
import { createDeferredHydration } from "./settings-hydration";

interface TestSettings {
  theme: string;
  fontSize: number;
  recentFiles: string[];
  showFileTree: boolean;
}

describe("deferred settings hydration", () => {
  it("replays early local mutations over loaded settings before writing", () => {
    const hydration = createDeferredHydration<TestSettings>();
    const defaults: TestSettings = {
      theme: "system",
      fontSize: 16,
      recentFiles: [],
      showFileTree: false,
    };
    const early = hydration.apply(defaults, (current) => ({
      ...current,
      recentFiles: ["opened.md", ...current.recentFiles],
    }));

    expect(early.recentFiles).toEqual(["opened.md"]);
    expect(hydration.requestPersistence()).toBe(false);

    const result = hydration.hydrate({
      theme: "dark",
      fontSize: 20,
      recentFiles: ["existing.md"],
      showFileTree: true,
    });

    expect(result).toEqual({
      value: {
        theme: "dark",
        fontSize: 20,
        recentFiles: ["opened.md", "existing.md"],
        showFileTree: true,
      },
      shouldPersist: true,
    });
    expect(hydration.requestPersistence()).toBe(true);
  });

  it("unblocks later persistence when loading settings fails", () => {
    const hydration = createDeferredHydration<TestSettings>();
    const defaults: TestSettings = {
      theme: "system",
      fontSize: 16,
      recentFiles: [],
      showFileTree: false,
    };
    const current = hydration.apply(defaults, (settings) => ({
      ...settings,
      theme: "dark",
    }));

    expect(hydration.requestPersistence()).toBe(false);
    expect(hydration.completeWithCurrent(current)).toEqual({
      value: {
        ...defaults,
        theme: "dark",
      },
      shouldPersist: true,
    });
    expect(hydration.requestPersistence()).toBe(true);
  });

  it("replays an early boolean choice as its visible target value", () => {
    const hydration = createDeferredHydration<TestSettings>();
    const defaults: TestSettings = {
      theme: "system",
      fontSize: 16,
      recentFiles: [],
      showFileTree: false,
    };
    const showFileTree = !defaults.showFileTree;
    const visible = hydration.apply(defaults, (current) => ({
      ...current,
      showFileTree,
    }));

    expect(visible.showFileTree).toBe(true);
    expect(hydration.requestPersistence()).toBe(false);

    const result = hydration.hydrate({
      ...defaults,
      showFileTree: true,
    });

    expect(result.value.showFileTree).toBe(true);
    expect(result.shouldPersist).toBe(true);
  });
});
