import { mount, tick, unmount } from "svelte";
import { describe, expect, it, vi } from "vitest";
import CommandPalette from "./CommandPalette.svelte";

describe("CommandPalette", () => {
  it("executes the button reached by keyboard focus", async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const first = vi.fn();
    const second = vi.fn();
    const component = mount(CommandPalette, { target, props: {
      open: true, commands: [
        { id: "first", label: "First", run: first },
        { id: "second", label: "Second", run: second },
      ], onClose: vi.fn(),
    } });
    try {
      await tick();
      await vi.waitFor(() => expect(document.activeElement).toBe(target.querySelector("input")));
      const button = target.querySelectorAll<HTMLButtonElement>(".command-list button")[1];
      // JSDOM has no native Tab traversal; reproduce its resulting focus.
      button.focus();
      await tick();
      button.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
      expect(first).not.toHaveBeenCalled();
      expect(second).toHaveBeenCalledOnce();
    } finally { await unmount(component); target.remove(); }
  });

  it("supports keyboard execution and defers filtering during IME composition", async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const run = vi.fn();
    const saveRun = vi.fn();
    const component = mount(CommandPalette, {
      target,
      props: {
        open: true,
        commands: [
          { id: "one", label: "打开文件", run },
          { id: "two", label: "保存文档", run: saveRun },
        ],
        onClose: vi.fn(),
      },
    });
    await tick();
    const input = target.querySelector("input")!;
    input.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    input.value = "保存";
    input.dispatchEvent(new InputEvent("input", { bubbles: true, data: "保存", isComposing: true }));
    await tick();
    expect(target.querySelectorAll(".command-list button")).toHaveLength(2);

    input.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true }));
    await tick();
    expect(target.querySelectorAll(".command-list button")).toHaveLength(1);

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(run).not.toHaveBeenCalled();
    expect(saveRun).toHaveBeenCalledOnce();
    await unmount(component);
    target.remove();
  });
});
