import { mount, tick, unmount } from "svelte";
import { describe, expect, it, vi } from "vitest";
import FileSidebar from "./FileSidebar.svelte";

describe("FileSidebar", () => {
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
