import { mount, tick, unmount } from "svelte";
import { describe, expect, it, vi } from "vitest";
import WorkspaceSearch from "./WorkspaceSearch.svelte";

const oldResult = {
  path: "C:\\notes\\old.md",
  relativePath: "old.md",
  line: 1,
  column: 1,
  preview: "old result",
};

const secondResult = {
  path: "C:\\notes\\second.md",
  relativePath: "second.md",
  line: 2,
  column: 3,
  preview: "second result",
};

describe("WorkspaceSearch", () => {
  it("does not expose results from the previous query to Enter", async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const onOpen = vi.fn();
    const component = mount(WorkspaceSearch, {
      target,
      props: {
        open: true,
        results: [oldResult],
        resultQuery: "old",
        onSearch: vi.fn(),
        onOpen,
        onClose: vi.fn(),
      },
    });
    await tick();

    const input = target.querySelector("input")!;
    input.value = "new";
    input.dispatchEvent(new InputEvent("input", { bubbles: true, data: "new" }));
    await tick();
    expect(target.querySelectorAll(".results button")).toHaveLength(0);

    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(onOpen).not.toHaveBeenCalled();

    await unmount(component);
    target.remove();
  });

  it("focuses the query and closes when the user clicks outside", async () => {
    const previous = document.createElement("button");
    const target = document.createElement("div");
    document.body.append(previous, target);
    previous.focus();
    const onClose = vi.fn();
    const component = mount(WorkspaceSearch, {
      target,
      props: {
        open: true,
        onSearch: vi.fn(),
        onOpen: vi.fn(),
        onClose,
      },
    });
    await tick();
    await Promise.resolve();

    expect(document.activeElement).toBe(target.querySelector("input"));
    document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(onClose).toHaveBeenCalledOnce();

    await unmount(component);
    expect(document.activeElement).toBe(previous);
    previous.remove();
    target.remove();
  });

  it("opens the result that received keyboard focus", async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const onOpen = vi.fn();
    const component = mount(WorkspaceSearch, {
      target,
      props: {
        open: true,
        results: [oldResult, secondResult],
        onSearch: vi.fn(),
        onOpen,
        onClose: vi.fn(),
      },
    });
    await tick();

    const buttons = target.querySelectorAll<HTMLButtonElement>(".results button");
    buttons[1].focus();
    buttons[1].dispatchEvent(new KeyboardEvent("keydown", {
      key: "Enter",
      bubbles: true,
      cancelable: true,
    }));
    expect(onOpen).toHaveBeenCalledWith(secondResult);

    await unmount(component);
    target.remove();
  });
});
