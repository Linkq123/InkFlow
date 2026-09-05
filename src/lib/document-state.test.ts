import { describe, expect, it } from "vitest";
import type { DocumentTab } from "./api/types";
import imageRewriteFixtures from "../../tests/fixtures/image-rewrites.json";
import imageRewriteMerges from "../../tests/fixtures/image-rewrite-merges.json";
import {
  applySavedResult,
  applyTextEdits,
  imageRewriteEditsBetween,
  isPathAffected,
  relocatedPath,
  replaceUploadPlaceholder,
  uploadPlaceholderEdit,
  withoutTabsById,
  textFromString,
} from "./document-state";

function tab(content: string): DocumentTab {
  return {
    id: "doc", path: "C:\\notes\\a.md", title: "a.md", content: textFromString(content),
    encoding: "utf-8", eol: "lf", hadBom: false, hadFinalNewline: false,
    readOnly: false, revision: null, dirty: true, saveState: "saving", mode: "live",
    externalChange: null, allowRemoteImages: false, editorVersion: 0,
  };
}

describe("document save state", () => {
  it.each(imageRewriteMerges)("merges migrated targets after syntax edits: $name", ({ saved, rewritten, current, expected }) => {
    const currentContent = `${current}\n\nnew input`;
    const result = applySavedResult(tab(currentContent), {
      status: "saved", path: "C:\\export\\Copy.md", revision: { hash: "new", size: 1, modifiedMs: 2 },
      content: rewritten, recoveryWarnings: [],
    }, saved, -1, currentContent);
    expect(result.tab.content.toString()).toBe(`${expected}\n\nnew input`);
    expect(result.tab.path).toBe("C:\\export\\Copy.md");
    expect(result.needsResave).toBe(true);
    const resaved = applySavedResult(result.tab, {
      status: "saved", path: result.tab.path!, revision: result.tab.revision!,
      content: null, recoveryWarnings: [],
    }, result.tab.content.toString(), result.tab.editorVersion, result.tab.content.toString());
    expect(resaved.tab.content.toString()).toBe(`${expected}\n\nnew input`);
    expect(resaved.tab.dirty).toBe(false);
  });

  it.each(imageRewriteFixtures)("preserves Save As rewrites and newer input: $name", ({ content, rewritten }) => {
    const currentContent = `${content}\n\nnew input`;
    const current = tab(currentContent);
    const saved = applySavedResult(current, {
      status: "saved", path: "C:\\export\\Copy.md", revision: { hash: "new", size: 1, modifiedMs: 2 },
      content: rewritten, recoveryWarnings: [],
    }, content, -1, currentContent);
    expect(saved.tab.content.toString()).toBe(`${rewritten}\n\nnew input`);
    expect(saved.tab.path).toBe("C:\\export\\Copy.md");
    expect(saved.needsResave).toBe(true);
    // The follow-up save must use the merged buffer, not the old paths.
    const nextRequest = saved.tab.content.toString();
    const resaved = applySavedResult(saved.tab, {
      status: "saved", path: saved.tab.path!, revision: { hash: "latest", size: nextRequest.length, modifiedMs: 3 },
      content: null, recoveryWarnings: [],
    }, nextRequest, saved.tab.editorVersion, nextRequest);
    expect(resaved.tab.content.toString()).toBe(`${rewritten}\n\nnew input`);
    expect(resaved.tab.dirty).toBe(false);
  });

  it("does not mix quoted and unquoted HTML replacement escaping", () => {
    const content = '<img src=old.png>\n<img src="old.png">';
    const rewritten = '<img src=My%20Note.assets/a.png>\n<img src="My Note.assets/a.png">';
    const result = applySavedResult(tab(`${content}\nnew input`), {
      status: "saved", path: "C:\\export\\My Note.md", revision: { hash: "new", size: 1, modifiedMs: 1 }, content: rewritten, recoveryWarnings: [],
    }, content, -1, `${content}\nnew input`);
    expect(result.tab.content.toString()).toBe(`${rewritten}\nnew input`);
  });

  it("does not guess when equivalent source targets have conflicting rewrites", () => {
    const saved = '<img src="images/a&amp;b.png">\n<img src="images/a&#38;b.png">';
    const rewritten = '<img src="Copy.assets/one.png">\n<img src="Copy.assets/two.png">';
    const current = "<img src='images/a&amp;b.png'>\nnew input";
    const result = applySavedResult(tab(current), {
      status: "saved", path: "C:\\export\\Copy.md", revision: { hash: "new", size: 1, modifiedMs: 2 },
      content: rewritten, recoveryWarnings: [],
    }, saved, -1, current);
    expect(result.tab.content.toString()).toBe(current);
    expect(result.needsResave).toBe(true);
  });

  it("keeps newer edits dirty when an older save completes", () => {
    const current = tab("old\nnew input");
    const result = applySavedResult(current, {
      status: "saved", path: current.path!, revision: { hash: "1", size: 3, modifiedMs: 1 }, content: null, recoveryWarnings: [],
    }, "old", -1, current.content.toString());
    expect(result.tab.content.toString()).toBe("old\nnew input");
    expect(result.tab.dirty).toBe(true);
    expect(result.needsResave).toBe(true);
  });

  it("marks the matching editor version saved without a second document snapshot", () => {
    const current = tab("unchanged");
    const result = applySavedResult(current, {
      status: "saved",
      path: current.path!,
      revision: { hash: "same", size: 9, modifiedMs: 1 },
      content: null,
      recoveryWarnings: [],
    }, "unchanged", current.editorVersion, "unchanged");

    expect(result.tab.content).toBe(current.content);
    expect(result.tab.dirty).toBe(false);
    expect(result.needsResave).toBe(false);
  });

  it("merges backend image-path rewrites into newer content", () => {
    const saved = "![x](inkflow-asset://x.png)";
    const current = tab(`${saved}\nnew input`);
    const result = applySavedResult(current, {
      status: "saved", path: current.path!, revision: { hash: "1", size: 3, modifiedMs: 1 },
      content: "![x](a.assets/x.png)", recoveryWarnings: [],
    }, saved, -1, current.content.toString());
    expect(result.tab.content.toString()).toBe("![x](a.assets/x.png)\nnew input");
    expect(result.tab.dirty).toBe(true);
    expect(result.tab.editorVersion).toBe(1);
  });

  it("merges reference and HTML image rewrites without changing normal definitions", () => {
    const saved = "![ref]\n\n[ref]: old/ref.png\n[link]: old/ref.png\n<img src=\"old/html.png\">";
    const rewritten = "![ref]\n\n[ref]: new/ref.png\n[link]: old/ref.png\n<img src=\"new/html.png\">";
    const result = applySavedResult(tab(`${saved}\nnew input`), {
      status: "saved", path: "C:\\notes\\b.md", revision: { hash: "2", size: 4, modifiedMs: 2 }, content: rewritten, recoveryWarnings: [],
    }, saved, -1, `${saved}\nnew input`);
    expect(result.tab.content.toString()).toContain("[ref]: new/ref.png");
    expect(result.tab.content.toString()).toContain("[link]: old/ref.png");
    expect(result.tab.content.toString()).toContain("src=\"new/html.png\"");
  });

  it("does not rewrite code or escaped examples that share an image path", () => {
    const oldPath = "draft.assets/image.png";
    const newPath = "published.assets/image.png";
    const saved = [
      `\`![inline](${oldPath})\``,
      "```markdown",
      `![fenced](${oldPath})`,
      "```",
      `\\![escaped](${oldPath})`,
      `![actual](${oldPath})`,
    ].join("\n");
    const rewritten = [
      `\`![inline](${oldPath})\``,
      "```markdown",
      `![fenced](${oldPath})`,
      "```",
      `\\![escaped](${oldPath})`,
      `![actual](${newPath})`,
    ].join("\n");
    const result = applySavedResult(tab(`${saved}\nnew input`), {
      status: "saved",
      path: "C:\\notes\\published.md",
      revision: { hash: "3", size: 5, modifiedMs: 3 },
      content: rewritten,
      recoveryWarnings: [],
    }, saved, -1, `${saved}\nnew input`);

    expect(result.tab.content.toString()).toBe(`${rewritten}\nnew input`);
    expect(result.tab.content.toString()).toContain(`\`![inline](${oldPath})\``);
    expect(result.tab.content.toString()).toContain(`![fenced](${oldPath})`);
    expect(result.tab.content.toString()).toContain(`\\![escaped](${oldPath})`);
  });

  it("derives occurrence-specific history edits without rewriting code examples", () => {
    const pending = "inkflow-asset://image.png";
    const saved = "note.assets/image.png";
    const before = `\`\`\`md\n![example](${pending})\n\`\`\`\n\n![actual](${pending})`;
    const after = `\`\`\`md\n![example](${pending})\n\`\`\`\n\n![actual](${saved})`;
    const edits = imageRewriteEditsBetween(before, after);

    expect(edits).toHaveLength(1);
    expect(applyTextEdits(before, edits)).toBe(after);
    expect(before.slice(edits[0].from, edits[0].to)).toBe(pending);
    expect(edits[0].from).toBe(before.lastIndexOf(pending));
  });

  it("preserves history rewrites when a saved asset path needs angle brackets", () => {
    const pending = "inkflow-asset://image.png";
    const before = `start ![x](${pending})`;
    const after = "start ![x](<My Note.assets/image.png>)";
    const edits = imageRewriteEditsBetween(before, after);

    expect(edits).toEqual([{
      from: before.indexOf(pending),
      to: before.indexOf(pending) + pending.length,
      insert: "<My Note.assets/image.png>",
    }]);
    expect(applyTextEdits(before, edits)).toBe(after);
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

  it("exposes the exact placeholder edit for editor-history rebasing", () => {
    const content = "before TOKEN after";
    const edit = uploadPlaceholderEdit(content, "TOKEN", "");

    expect(edit).toEqual({ from: 7, to: 12, insert: "" });
    expect(applyTextEdits(content, edit ? [edit] : [])).toBe("before  after");
  });
});

describe("tab removal", () => {
  it("removes saved replacements by ID instead of object identity", () => {
    const beforeSave = tab("dirty");
    const afterSave = { ...beforeSave, content: textFromString("saved"), dirty: false };
    expect(withoutTabsById([afterSave], [beforeSave.id])).toEqual([]);
  });
});
