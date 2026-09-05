import { mount, tick, unmount } from "svelte";
import { describe, expect, it, vi } from "vitest";
import App from "./App.svelte";

describe("InkFlow shell", () => {
  it("shows a minimal empty-state guide and dismisses it for writing", async () => {
    vi.stubGlobal("matchMedia", () => ({
      matches: false,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    }));
    const target = document.createElement("div");
    document.body.append(target);
    const component = mount(App, { target });
    await tick();

    const welcome = target.querySelector<HTMLElement>(".welcome-card");
    expect(welcome?.textContent).toContain("Start writing");
    const logo = welcome?.querySelector<HTMLImageElement>(".welcome-mark img");
    expect(logo).not.toBeNull();
    expect(logo?.getAttribute("alt")).toBe("");
    expect(logo?.closest(".welcome-mark")?.getAttribute("aria-hidden")).toBe("true");
    welcome?.querySelector<HTMLButtonElement>("button.primary")?.click();
    await tick();
    expect(target.querySelector(".welcome-card")).toBeNull();
    expect(target.querySelector(".welcome-mark img")).toBeNull();

    await unmount(component);
    target.remove();
    vi.unstubAllGlobals();
  });
});
