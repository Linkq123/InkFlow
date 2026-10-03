import { EditorSelection, EditorState, type TransactionSpec } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { describe, expect, it, vi } from "vitest";
import { formatSelection, replaceCurrentLine } from "./commands";
import { renderMarkdown } from "../markdown/pipeline";

function mockView(doc: string, anchor: number, head = anchor, readOnly = false): EditorView {
  let state = EditorState.create({
    doc,
    selection: EditorSelection.single(anchor, head),
    extensions: [markdown({ base: markdownLanguage }), ...(readOnly ? [EditorState.readOnly.of(true)] : [])],
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
  it.each([
    ["prefix ", " suffix"],
    ["**prefix ", " suffix**"],
  ])("formats multiline code inside its actual paragraph: %s", async (prefix, suffix) => {
    const selected = "a\n``b";
    const source = prefix + selected + suffix + "\n\nKeep **tail**";
    const from = prefix.length, to = from + selected.length;
    const view = mockView(source, to, from);
    expect(formatSelection(view, "code")).toBe(true);
    const selection = view.state.selection.main;
    expect(selection.anchor).toBeGreaterThan(selection.head);
    expect(view.state.sliceDoc(selection.from, selection.to)).toBe(selected);
    const html = new DOMParser().parseFromString(await renderMarkdown(view.state.doc.toString()), "text/html");
    expect(html.querySelectorAll("code")).toHaveLength(1);
    expect(html.querySelector("code")?.textContent).toBe("a ``b");
    expect(html.querySelector("pre")).toBeNull();
    expect(html.querySelector("p:first-child")?.textContent).toBe("prefix a ``b suffix");
    expect(html.querySelector("p:last-child strong")?.textContent).toBe("tail");
  });

  it.each([
    ["\\`literal\\`", "`literal`", null],
    ["before \\`array[0]&value\\` after", "before `array[0]&value` after", null],
    ["\\`single", "`single", null],
    ["\\`literal\\` and `array[0]`", "`literal` and array[0]", "array[0]"],
  ] as const)("keeps escaped backticks literal when inserting a link: %s", async (selected, text, code) => {
    const view = mockView(selected, 0, selected.length);
    expect(formatSelection(view, "link")).toBe(true);
    const selection = view.state.selection.main;
    expect(view.state.sliceDoc(selection.from, selection.to)).toBe("https://");
    view.dispatch({ changes: { from: selection.from, to: selection.to, insert: "https://example.com" } });
    const html = new DOMParser().parseFromString(await renderMarkdown(view.state.doc.toString()), "text/html");
    expect(html.querySelectorAll("a")).toHaveLength(1);
    expect(html.querySelector("a")?.textContent).toBe(text);
    expect(html.querySelector("a code")?.textContent ?? null).toBe(code);
  });

  it.each(["a\n``b", "a\nb```c"])("refuses multiline code that would consume following prose: %s", async selected => {
    const source = selected + "\n\nKeep **tail**";
    const view = mockView(source, selected.length, 0);
    const beforeSelection = view.state.selection;
    expect(formatSelection(view, "code")).toBe(false);
    expect(view.state.doc.toString()).toBe(source);
    expect(view.state.selection.eq(beforeSelection)).toBe(true);
    const html = new DOMParser().parseFromString(await renderMarkdown(view.state.doc.toString()), "text/html");
    expect(html.querySelector("pre")).toBeNull();
    expect(html.querySelector("p:last-child")?.textContent).toBe("Keep tail");
    expect(html.querySelector("p:last-child strong")?.textContent).toBe("tail");
  });

  it("keeps safe multiline code inline and leaves the following paragraph alone", async () => {
    const selected = "a\nb";
    const view = mockView(selected + "\n\nKeep **tail**", 0, selected.length);
    expect(formatSelection(view, "code")).toBe(true);
    const html = new DOMParser().parseFromString(await renderMarkdown(view.state.doc.toString()), "text/html");
    expect(html.querySelector("pre")).toBeNull();
    expect(html.querySelector("p code")?.textContent).toBe("a b");
    expect(html.querySelector("p:last-child strong")?.textContent).toBe("tail");
  });

  it.each([
    ["`array[0]`", "array[0]", "array[0]"],
    ["`A&B`", "A&B", "A&B"],
    ["``array[`index`] & value``", "array[`index`] & value", "array[`index`] & value"],
    ["before] `A&B` after[", "before] A&B after[", "A&B"],
  ])("preserves code spans inside a link label: %s", async (selected, text, code) => {
    const view = mockView(selected, 0, selected.length);
    expect(formatSelection(view, "link")).toBe(true);
    const selection = view.state.selection.main;
    expect(view.state.sliceDoc(selection.from, selection.to)).toBe("https://");
    view.dispatch({ changes: { from: selection.from, to: selection.to, insert: "https://example.com" } });
    const html = new DOMParser().parseFromString(await renderMarkdown(view.state.doc.toString()), "text/html");
    expect(html.querySelectorAll("a")).toHaveLength(1);
    expect(html.querySelector("a")?.textContent).toBe(text);
    expect(html.querySelector("a code")?.textContent).toBe(code);
    expect(html.querySelector("a")?.getAttribute("href")).toBe("https://example.com");
  });

  it.each(["echo `pwd`", "`edge", "edge`", "two ``ticks``", " both sides ", "   "])("preserves inline code text: %s", async selected => {
    const view = mockView(selected, 0, selected.length);
    expect(formatSelection(view, "code")).toBe(true);
    const html = new DOMParser().parseFromString(await renderMarkdown(view.state.doc.toString()), "text/html");
    expect(html.querySelectorAll("code")).toHaveLength(1);
    expect(html.querySelector("code")?.textContent).toBe(selected);
    expect(view.state.sliceDoc(view.state.selection.main.from, view.state.selection.main.to)).toBe(selected);
  });

  it.each(["photo]", "photo[", String.raw`photo\]`, "photo&amp;"])("escapes a literal link label: %s", async selected => {
    const view = mockView(selected, 0, selected.length);
    formatSelection(view, "link");
    const selection = view.state.selection.main;
    expect(view.state.sliceDoc(selection.from, selection.to)).toBe("https://");
    view.dispatch({ changes: { from: selection.from, to: selection.to, insert: "https://example.com" } });
    const html = new DOMParser().parseFromString(await renderMarkdown(view.state.doc.toString()), "text/html");
    expect(html.querySelectorAll("a")).toHaveLength(1);
    expect(html.querySelector("a")?.textContent).toBe(selected);
  });
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
