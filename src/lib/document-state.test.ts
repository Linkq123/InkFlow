import { describe, expect, it } from "vitest";
import type { DocumentTab } from "./api/types";
import { applySavedResult, isPathAffected, relocatedPath, replaceUploadPlaceholder, withoutTabsById } from "./document-state";

function tab(content: string): DocumentTab {
  return {
    id: "doc", path: "C:\\notes\\a.md", title: "a.md", content,
    encoding: "utf-8", eol: "lf", hadBom: false, hadFinalNewline: false,
    readOnly: false, revision: null, dirty: true, saveState: "saving", mode: "live",
    externalChange: null, allowRemoteImages: false,
  };
}

describe("document save state", () => {
  it("keeps newer edits dirty when an older save completes", () => {
    const current = tab("old\nnew input");
    const result = applySavedResult(current, {
      status: "saved", path: current.path!, revision: { hash: "1", size: 3, modifiedMs: 1 }, content: null,
    }, "old");
    expect(result.tab.content).toBe("old\nnew input");
    expect(result.tab.dirty).toBe(true);
    expect(result.needsResave).toBe(true);
  });

  it("merges backend image-path rewrites into newer content", () => {
    const saved = "![x](inkflow-asset://x.png)";
    const current = tab(`${saved}\nnew input`);
    const result = applySavedResult(current, {
      status: "saved", path: current.path!, revision: { hash: "1", size: 3, modifiedMs: 1 },
      content: "![x](a.assets/x.png)",
    }, saved);
    expect(result.tab.content).toBe("![x](a.assets/x.png)\nnew input");
    expect(result.tab.dirty).toBe(true);
  });

  it("merges reference and HTML image rewrites without changing normal definitions", () => {
    const saved = "![ref]\n[ref]: old/ref.png\n[link]: old/ref.png\n<img src=\"old/html.png\">";
    const rewritten = "![ref]\n[ref]: new/ref.png\n[link]: old/ref.png\n<img src=\"new/html.png\">";
    const result = applySavedResult(tab(`${saved}\nnew input`), {
      status: "saved", path: "C:\\notes\\b.md", revision: { hash: "2", size: 4, modifiedMs: 2 }, content: rewritten,
    }, saved);
    expect(result.tab.content).toContain("[ref]: new/ref.png");
    expect(result.tab.content).toContain("[link]: old/ref.png");
    expect(result.tab.content).toContain("src=\"new/html.png\"");
  });
});

describe("workspace path matching", () => {
  it("does not treat similarly prefixed sibling directories as descendants", () => {
    expect(isPathAffected("C:\\notes\\foobar\\a.md", "C:\\notes\\foo", true)).toBe(false);
    expect(isPathAffected("C:\\notes\\foo\\a.md", "C:\\notes\\foo", true)).toBe(true);
  });

  it("relocates directory descendants without changing siblings", () => {
    expect(relocatedPath("C:\\notes\\foo\\a.md", "C:\\notes\\foo", "C:\\notes\\bar", true))
      .toBe("C:\\notes\\bar\\a.md");
  });
});

describe("image upload placeholders", () => {
  it("only replaces the placeholder in the document that still contains it", () => {
    expect(replaceUploadPlaceholder("before TOKEN after", "TOKEN", "![x](x.png)"))
      .toBe("before ![x](x.png) after");
    expect(replaceUploadPlaceholder("placeholder was undone", "TOKEN", "![x](x.png)"))
      .toBeNull();
  });
});

describe("tab removal", () => {
  it("removes saved replacements by ID instead of object identity", () => {
    const beforeSave = tab("dirty");
    const afterSave = { ...beforeSave, content: "saved", dirty: false };
    expect(withoutTabsById([afterSave], [beforeSave.id])).toEqual([]);
  });
});
