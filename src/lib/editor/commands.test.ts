import { EditorSelection, EditorState, type TransactionSpec } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { describe, expect, it, vi } from "vitest";
import { formatSelection, replaceCurrentLine } from "./commands";
import { renderMarkdown } from "../markdown/pipeline";

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
  it.each([["bold", "strong"], ["italic", "em"], ["strike", "del"]] as const)("keeps edge whitespace outside %s and preserves reverse selection", async (format, tag) => {
    const view = mockView(" hello \n", 8, 0);
    expect(formatSelection(view, format)).toBe(true);
    expect(view.state.doc.toString()).toMatch(/^ .* \n$/);
    const selection = view.state.selection.main;
    expect(selection.anchor).toBeGreaterThan(selection.head);
    expect(view.state.sliceDoc(selection.from, selection.to)).toBe("hello");
    const html = new DOMParser().parseFromString(await renderMarkdown(view.state.doc.toString()), "text/html");
    expect(html.querySelector(tag)?.textContent).toBe("hello");
  });

  it.each(["bold", "italic", "strike"] as const)("leaves a whitespace-only %s selection unchanged", format => {
    const view = mockView(" \t ", 0, 3);
    expect(formatSelection(view, format)).toBe(false);
    expect(view.state.doc.toString()).toBe(" \t ");
    expect(view.state.selection.main.to).toBe(3);
  });
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

  it("preserves text after the slash-command cursor", () => {
    const view = mockView("/heading Original paragraph", 8);
    replaceCurrentLine(view, "# ");
    expect(view.state.doc.toString()).toBe("#  Original paragraph");
  });
});
