import { mount, tick, unmount } from "svelte";
import { describe, expect, it } from "vitest";
import SettingsDialog from "./SettingsDialog.svelte";
import type { SettingsV1 } from "../api/types";

const settings: SettingsV1 = {
  schemaVersion: 1,
  theme: "system",
  locale: "system",
  fontSize: 16,
  pageWidth: 820,
  lineHeight: 1.75,
  editorFont: "Segoe UI",
  codeFont: "Cascadia Mono",
  autosaveDelayMs: 750,
  showFileTree: false,
  showOutline: false,
  focusMode: false,
  typewriterMode: false,
  recentFiles: [],
  recentWorkspaces: [],
};

describe("SettingsDialog", () => {
  it("initializes its draft when opened", async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const component = mount(SettingsDialog, {
      target,
      props: {
        open: true,
        locale: "zh-CN",
        settings,
        onSave: () => undefined,
        onClose: () => undefined,
      },
    });
    await tick();

    expect(target.querySelector('[role="dialog"]')).not.toBeNull();
    expect(target.textContent).toContain("设置");

    await unmount(component);
    target.remove();
  });
});
