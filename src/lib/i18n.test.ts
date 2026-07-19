import { describe, expect, it } from "vitest";
import { translate } from "./i18n";

describe("i18n", () => {
  it("renders both interface languages", () => {
    expect(translate("zh-CN", "save")).toBe("保存");
    expect(translate("en-US", "save")).toBe("Save");
  });

  it("interpolates dialog values without altering missing placeholders", () => {
    expect(translate("en-US", "confirmSave", { title: "Draft.md" })).toBe(
      "Save changes to “Draft.md”?",
    );
  });
});
