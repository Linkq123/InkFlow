import { history, isolateHistory, redo, undo } from "@codemirror/commands";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { describe, expect, it, vi } from "vitest";
import { imageRewriteEdits } from "../document-state";
import {
  cacheEditorState,
  createCachedEditorState,
  rebaseCachedEditorState,
  rebaseEditorState,
} from "./state-cache";

function viewFor(state: EditorState): EditorView {
  return new EditorView({ state, parent: document.createElement("div") });
}

describe("editor state cache", () => {
  it("preserves undo history across editor recreation", () => {
    let state = createCachedEditorState("first", [history()], null, 0);
    state = state.update({ changes: { from: state.doc.length, insert: " second" } }).state;
    const restored = createCachedEditorState(
      state.doc.toString(),
      [history()],
      cacheEditorState(state, 1),
      1,
    );
    const view = viewFor(restored);

    expect(view.state.doc.toString()).toBe("first second");
    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("first");
    view.destroy();
  });

  it("does not serialize or retain a second copy of an unchanged long document", () => {
    const documentValue = "x".repeat(1024 * 1024);
    const state = createCachedEditorState(documentValue, [history()], null, 0);
    const toJson = vi.spyOn(EditorState.prototype, "toJSON");
    const cached = cacheEditorState(state, 0);

    expect(toJson).not.toHaveBeenCalled();
    expect(cached).not.toHaveProperty("doc");
    expect(JSON.stringify(cached).length).toBeLessThan(2_000);
    toJson.mockRestore();
  });

  it("drops incompatible history when the document version changed while hidden", () => {
    let state = createCachedEditorState("first", [history()], null, 0);
    state = state.update({ changes: { from: state.doc.length, insert: " second" } }).state;
    const restored = createCachedEditorState(
      "external",
      [history()],
      cacheEditorState(state, 1),
      2,
    );
    const view = viewFor(restored);

    expect(view.state.doc.toString()).toBe("external");
    expect(undo(view)).toBe(false);
    view.destroy();
  });

  it("rebases undo and redo history through an asset destination rewrite", () => {
    const oldPath = "inkflow-asset://image.png";
    const newPath = "note.assets/image.png";
    const rewrite = (doc: string) => doc.replaceAll(oldPath, newPath);
    let state = EditorState.create({ doc: "start", extensions: [history()] });
    state = state.update({
      changes: { from: state.doc.length, insert: ` ![x](${oldPath})` },
      annotations: isolateHistory.of("full"),
    }).state;
    state = state.update({
      changes: { from: 0, insert: "edited " },
      annotations: isolateHistory.of("full"),
    }).state;
    const current = state.doc.toString();
    const from = current.indexOf(oldPath);
    const rebased = rebaseEditorState(
      state,
      rewrite(current),
      [history()],
      [{ from, to: from + oldPath.length, insert: newPath }],
    );
    const view = viewFor(rebased);

    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe(`start ![x](${newPath})`);
    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("start");
    expect(redo(view)).toBe(true);
    expect(redo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe(`edited start ![x](${newPath})`);
    view.destroy();
  });

  it("uses the save-result rewrite rules when rebasing editor history", () => {
    const placeholder = "inkflow-asset://image.png";
    const savedPath = "<My Note.assets/image.png>";
    let state = EditorState.create({ doc: "start", extensions: [history()] });
    state = state.update({
      changes: { from: state.doc.length, insert: ` ![x](${placeholder})` },
      annotations: isolateHistory.of("full"),
    }).state;
    const before = state.doc.toString();
    const after = `start ![x](${savedPath})`;
    const rebased = rebaseEditorState(
      state,
      after,
      [history()],
      imageRewriteEdits(before, before, after),
    );
    const view = viewFor(rebased);

    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("start");
    expect(redo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe(after);
    view.destroy();
  });

  it("drops a failed upload placeholder without dropping earlier undo history", () => {
    const placeholder = " ![x](inkflow-upload://image)";
    let state = EditorState.create({ doc: "start", extensions: [history()] });
    state = state.update({
      changes: { from: state.doc.length, insert: " edited" },
      annotations: isolateHistory.of("full"),
    }).state;
    state = state.update({
      changes: { from: state.doc.length, insert: placeholder },
      annotations: isolateHistory.of("full"),
    }).state;
    const from = state.doc.length - placeholder.length;
    const rebased = rebaseEditorState(
      state,
      "start edited",
      [history()],
      [{ from, to: state.doc.length, insert: "" }],
    );
    const view = viewFor(rebased);

    expect(view.state.doc.toString()).toBe("start edited");
    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("start");
    expect(redo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("start edited");
    expect(view.state.doc.toString()).not.toContain("inkflow-upload://");
    view.destroy();
  });

  it("preserves an existing redo branch while rewriting an asset path", () => {
    const oldPath = "inkflow-asset://image.png";
    const newPath = "note.assets/image.png";
    let state = EditorState.create({ doc: "start", extensions: [history()] });
    state = state.update({
      changes: { from: state.doc.length, insert: ` ![x](${oldPath})` },
      annotations: isolateHistory.of("full"),
    }).state;
    state = state.update({
      changes: { from: state.doc.length, insert: " later" },
      annotations: isolateHistory.of("full"),
    }).state;
    const beforeRewrite = viewFor(state);
    expect(undo(beforeRewrite)).toBe(true);
    state = beforeRewrite.state;
    beforeRewrite.destroy();

    const before = state.doc.toString();
    const after = before.replace(oldPath, newPath);
    const from = before.indexOf(oldPath);
    const rebased = rebaseEditorState(
      state,
      after,
      [history()],
      [{ from, to: from + oldPath.length, insert: newPath }],
    );
    const view = viewFor(rebased);

    expect(redo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe(`${after} later`);
    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe(after);
    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("start");
    view.destroy();
  });

  it("rebases a hidden editor cache without storing its document", () => {
    const before = "start ![x](old.png)";
    let state = EditorState.create({ doc: "start", extensions: [history()] });
    state = state.update({
      changes: { from: state.doc.length, insert: " ![x](old.png)" },
    }).state;
    const cached = rebaseCachedEditorState(
      cacheEditorState(state, 1),
      before,
      1,
      "start ![x](new.png)",
      2,
      [{
        from: before.indexOf("old.png"),
        to: before.indexOf("old.png") + "old.png".length,
        insert: "new.png",
      }],
    );
    const restored = createCachedEditorState(
      "start ![x](new.png)",
      [history()],
      cached,
      2,
    );
    const view = viewFor(restored);

    expect(undo(view)).toBe(true);
    expect(view.state.doc.toString()).toBe("start");
    expect(cached).not.toHaveProperty("doc");
    view.destroy();
  });
});
