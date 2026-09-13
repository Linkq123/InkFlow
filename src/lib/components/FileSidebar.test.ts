import { mount, tick, unmount } from "svelte";
import { describe, expect, it, vi } from "vitest";
import FileSidebar from "./FileSidebar.svelte";

describe("FileSidebar", () => {
  it.each(["mouse", "keyboard"])("updates nested entries immediately when toggled with %s", async input => {
    const target = document.createElement("div");
    document.body.append(target);
    const entries = [
      { name: "docs", path: "C:\\notes\\docs", isDir: true, depth: 0 },
      { name: "nested", path: "C:\\notes\\docs\\nested", isDir: true, depth: 1 },
      { name: "a.md", path: "C:\\notes\\docs\\nested\\a.md", isDir: false, depth: 2 },
      { name: "readme.md", path: "C:\\notes\\docs\\readme.md", isDir: false, depth: 1 },
      { name: "other.md", path: "C:\\notes\\other.md", isDir: false, depth: 0 },
    ];
    const component = mount(FileSidebar, {
      target,
      props: {
        workspace: { root: "C:\\notes", name: "notes", entries },
        onOpen: vi.fn(), onCreate: vi.fn(), onRefresh: vi.fn(), onRename: vi.fn(), onDelete: vi.fn(),
      },
    });
    const visible = () => Array.from(target.querySelectorAll(".file-main")).map(item => item.textContent?.trim());
    const toggle = async (name: string, expand: boolean) => {
      const button = Array.from(target.querySelectorAll<HTMLButtonElement>(".file-main"))
        .find(item => item.textContent?.trim() === name)!;
      if (input === "mouse") button.click();
      else button.dispatchEvent(new KeyboardEvent("keydown", { key: expand ? "ArrowRight" : "ArrowLeft", bubbles: true, cancelable: true }));
      await tick();
      expect(button.getAttribute("aria-expanded")).toBe(String(expand));
    };
    try {
      await tick();
      expect(visible()).toEqual(["docs", "nested", "a.md", "readme.md", "other.md"]);
      await toggle("nested", false);
      expect(visible()).toEqual(["docs", "nested", "readme.md", "other.md"]);
      await toggle("docs", false);
      expect(visible()).toEqual(["docs", "other.md"]);
      await toggle("docs", true);
      expect(visible()).toEqual(["docs", "nested", "readme.md", "other.md"]);
      await toggle("nested", true);
      expect(visible()).toEqual(["docs", "nested", "a.md", "readme.md", "other.md"]);
    } finally { await unmount(component); target.remove(); }
  });

  it("navigates its entry menu with the keyboard and restores focus", async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const component = mount(FileSidebar, {
      target,
      props: {
        locale: "en-US",
        workspace: {
          root: "C:\\notes",
          name: "notes",
          entries: [{
            name: "draft.md",
            path: "C:\\notes\\draft.md",
            isDir: false,
            depth: 0,
          }],
        },
        onOpen: vi.fn(),
        onCreate: vi.fn(),
        onRefresh: vi.fn(),
        onRename: vi.fn(),
        onDelete: vi.fn(),
      },
    });
    await tick();

    const trigger = target.querySelector<HTMLButtonElement>(".row-menu")!;
    trigger.focus();
    trigger.click();
    await tick();

    const items = target.querySelectorAll<HTMLButtonElement>('[role="menuitem"]');
    expect(items).toHaveLength(2);
    expect(document.activeElement).toBe(items[0]);

    items[0].dispatchEvent(new KeyboardEvent("keydown", {
      key: "ArrowDown",
      bubbles: true,
      cancelable: true,
    }));
    expect(document.activeElement).toBe(items[1]);

    items[1].dispatchEvent(new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    }));
    await tick();
    expect(target.querySelector('[role="menu"]')).toBeNull();
    expect(document.activeElement).toBe(trigger);

    await unmount(component);
    target.remove();
  });
});
