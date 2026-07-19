import type { EditorView } from "@codemirror/view";

export type FormatName = "bold" | "italic" | "strike" | "code" | "link";

export function formatSelection(view: EditorView, format: FormatName): boolean {
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
  view.dispatch({
    changes: { from: selection.from, to: selection.to, insert: `${before}${selected}${after}` },
    selection: selected
      ? { anchor: selection.from + before.length, head: selection.to + before.length }
      : { anchor: selection.from + before.length },
    userEvent: "input.format",
  });
  return true;
}

export function replaceCurrentLine(view: EditorView, prefix: string): void {
  const line = view.state.doc.lineAt(view.state.selection.main.head);
  view.dispatch({
    changes: { from: line.from, to: line.to, insert: prefix },
    selection: { anchor: line.from + prefix.length },
    userEvent: "input.slash-command",
  });
  view.focus();
}

