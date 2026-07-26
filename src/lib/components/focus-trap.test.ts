import { describe, expect, it, vi } from "vitest";
import { focusTrap } from "./focus-trap";

describe("focus trap", () => {
  it("closes on Escape and restores the previous focus", async () => {
    const previous = document.createElement("button");
    const dialog = document.createElement("div");
    const input = document.createElement("input");
    dialog.append(input);
    document.body.append(previous, dialog);
    previous.focus();
    const onClose = vi.fn();
    const action = focusTrap(dialog, { onClose, initialFocus: "input" });
    await Promise.resolve();

    expect(document.activeElement).toBe(input);
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(onClose).toHaveBeenCalledOnce();

    action.destroy();
    expect(document.activeElement).toBe(previous);
    previous.remove();
    dialog.remove();
  });
});
