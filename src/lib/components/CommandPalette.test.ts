import { mount, tick, unmount } from "svelte";
import { describe, expect, it, vi } from "vitest";
import CommandPalette from "./CommandPalette.svelte";

describe("CommandPalette", () => {
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
