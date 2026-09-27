import type { EditorView } from "@codemirror/view";

export type FormatName = "bold" | "italic" | "strike" | "code" | "link";

export function formatSelection(view: EditorView, format: FormatName): boolean {
  if (view.state.readOnly) return false;
  const wrappers: Record<Exclude<FormatName, "link">, [string, string]> = {
    bold: ["**", "**"],
    italic: ["*", "*"],
    strike: ["~~", "~~"],
    code: ["`", "`"],
  };
  const selection = view.state.selection.main;
  if (format === "link") {
    const selected = view.state.sliceDoc(selection.from, selection.to) || "link";
    view.dispatch({
      changes: { from: selection.from, to: selection.to, insert: `[${selected}](https://)` },
      selection: { anchor: selection.from + selected.length + 3, head: selection.from + selected.length + 11 },
      userEvent: "input.format",
    });
    return true;
  }
  const [before, after] = wrappers[format];
  const selected = view.state.sliceDoc(selection.from, selection.to);
  const emphasis = format === "bold" || format === "italic" || format === "strike";
  const leading = emphasis ? selected.length - selected.trimStart().length : 0;
  const core = emphasis ? selected.trim() : selected;
  if (selected && emphasis && !core) return false;
  const from = selection.from + leading;
  const to = from + core.length;
  view.dispatch({
    changes: { from, to, insert: `${before}${core}${after}` },
    selection: selected
      ? selection.anchor <= selection.head
        ? { anchor: from + before.length, head: to + before.length }
        : { anchor: to + before.length, head: from + before.length }
      : { anchor: selection.from + before.length },
    userEvent: "input.format",
  });
  return true;
}

export function replaceCurrentLine(view: EditorView, prefix: string): void {
  if (view.state.readOnly) return;
  const line = view.state.doc.lineAt(view.state.selection.main.head);
  view.dispatch({
    // The menu matches only the text before the caret. Existing text after it
    // belongs to the document, not to the slash command being replaced.
    changes: { from: line.from, to: view.state.selection.main.head, insert: prefix },
    selection: { anchor: line.from + prefix.length },
    userEvent: "input.slash-command",
  });
  view.focus();
}
