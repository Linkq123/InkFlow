import type { EditorView } from "@codemirror/view";
import { commonmarkLanguage } from "@codemirror/lang-markdown";
import { ensureSyntaxTree } from "@codemirror/language";

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
    const label = escapeLinkLabel(selected);
    const replacement = `[${label}](https://)`;
    const link = commonmarkLanguage.parser.parse(replacement).topNode.firstChild?.getChild("Link");
    if (link?.from !== 0 || link.to !== replacement.length) return false;
    view.dispatch({
      changes: { from: selection.from, to: selection.to, insert: replacement },
      selection: { anchor: selection.from + label.length + 3, head: selection.from + label.length + 11 },
      userEvent: "input.format",
    });
    return true;
  }
  let [before, after] = wrappers[format];
  const selected = view.state.sliceDoc(selection.from, selection.to);
  if (format === "code" && selected) {
    let longest = 0;
    for (const match of selected.matchAll(/`+/g)) longest = Math.max(longest, match[0].length);
    const fence = "`".repeat(longest + 1);
    const padding = selected.startsWith("`") || selected.endsWith("`")
      || (selected.startsWith(" ") && selected.endsWith(" ") && !/^ +$/.test(selected));
    before = fence + (padding ? " " : "");
    after = (padding ? " " : "") + fence;
    if (selected.includes("\n")) {
      if (!isInlineCodeReplacement(view, before + selected + after)) return false;
    }
  }
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

function isInlineCodeReplacement(view: EditorView, replacement: string): boolean {
  const { from, to } = view.state.selection.main;
  // Use an undispatched candidate state to preserve paragraph/container context
  // and reuse the incremental parser. A parse timeout leaves the document intact.
  const candidate = view.state.update({ changes: { from, to, insert: replacement } }).state;
  const end = from + replacement.length;
  const tree = ensureSyntaxTree(candidate, end, 50);
  let code = tree?.resolveInner(from, 1) ?? null;
  while (code && code.name !== "InlineCode") code = code.parent;
  if (code?.from !== from || code.to !== end) return false;
  for (let parent = code.parent; parent; parent = parent.parent) {
    if (["FencedCode", "CodeBlock", "HTMLBlock"].includes(parent.name)) return false;
  }
  return true;
}

function escapeLinkLabel(value: string): string {
  const escapeText = (text: string) => text.replace(/[\\[\]&`]/g,
    character => character === "&" ? "&amp;" : "\\" + character);
  if (!value.includes("`")) return escapeText(value);

  // Parse in label context so backticks are inline syntax, even at its start.
  // Inside a code span, escapes and entities would change the displayed text.
  // Preserve existing backtick escapes too, so they cannot become code markers.
  let label = "";
  let offset = 0;
  commonmarkLanguage.parser.parse("[" + value + "](https://)").iterate({
    enter(node) {
      if (node.from < 1 || node.to > value.length + 1) return;
      const from = node.from - 1, to = node.to - 1;
      if (node.name !== "InlineCode"
        && !(node.name === "Escape" && value.slice(from, to) === "\\`")) return;
      label += escapeText(value.slice(offset, from)) + value.slice(from, to);
      offset = to;
      return false;
    },
  });
  return label + escapeText(value.slice(offset));
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
