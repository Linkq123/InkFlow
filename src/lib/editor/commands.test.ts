import { EditorSelection, EditorState, type TransactionSpec } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { describe, expect, it, vi } from "vitest";
import { formatSelection, replaceCurrentLine } from "./commands";

function mockView(doc: string, anchor: number, head = anchor, readOnly = false): EditorView {
  let state = EditorState.create({
    doc,
    selection: EditorSelection.single(anchor, head),
    extensions: readOnly ? [EditorState.readOnly.of(true)] : [],
  });
  const view = {
    get state() {
      return state;
    },
    dispatch(spec: TransactionSpec) {
      state = state.update(spec).state;
    },
    focus: vi.fn(),
  };
  return view as unknown as EditorView;
}

describe("editor formatting commands", () => {
  it("wraps a selection and preserves the selected text", () => {
    const view = mockView("write InkFlow", 6, 13);
    formatSelection(view, "bold");
    expect(view.state.doc.toString()).toBe("write **InkFlow**");
    expect(view.state.sliceDoc(
      view.state.selection.main.from,
      view.state.selection.main.to,
    )).toBe("InkFlow");
  });

  it("inserts link syntax and selects the URL placeholder", () => {
    const view = mockView("InkFlow", 0, 7);
    formatSelection(view, "link");
    expect(view.state.doc.toString()).toBe("[InkFlow](https://)");
    expect(view.state.sliceDoc(
      view.state.selection.main.from,
      view.state.selection.main.to,
    )).toBe("https://");
  });

  it("replaces the current slash-command line", () => {
    const view = mockView("first\n/", 7);
    replaceCurrentLine(view, "## ");
    expect(view.state.doc.toString()).toBe("first\n## ");
  });

  it("does not modify a read-only editor", () => {
    const view = mockView("locked", 0, 6, true);

    expect(formatSelection(view, "bold")).toBe(false);
    replaceCurrentLine(view, "# ");
    expect(view.state.doc.toString()).toBe("locked");
  });
});
