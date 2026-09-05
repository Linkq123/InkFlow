import { syntaxTree } from "@codemirror/language";
import { StateEffect, StateField, type EditorState, type Extension } from "@codemirror/state";
import { Decoration, EditorView, ViewPlugin, WidgetType, type DecorationSet, type ViewUpdate } from "@codemirror/view";
import {
  blockRemoteImageRequests,
  decodeMarkdownResourceDestination,
  hasRemoteMermaidImageReference,
  isRemoteImageSource,
  resolveLocalMermaidImageReferences,
} from "../markdown/resources";
import { renderMermaid } from "../markdown/mermaid-service";

const MAX_RENDERED_BLOCK_CHARS = 200_000;
const MAX_FALLBACK_FENCE_LINES = 500;
const MAX_FALLBACK_MATH_LINES = 500;
const MAX_TABLE_SCAN_LINES = 500;

async function renderMath(source: string, target: HTMLElement, displayMode: boolean): Promise<void> {
  try {
    const { default: katex } = await import("katex");
    katex.render(source, target, { displayMode, throwOnError: false, strict: "warn" });
  } catch {
    target.textContent = displayMode ? `$$\n${source}\n$$` : `$${source}$`;
  }
}

interface FusionOptions {
  documentId: string;
  loadResource: (documentId: string, source: string) => Promise<string>;
  allowRemoteImages: boolean;
}

class CheckboxWidget extends WidgetType {
  constructor(
    readonly checked: boolean,
    readonly from: number,
    readonly disabled: boolean,
    readonly toggle: (from: number, checked: boolean) => void,
  ) {
    super();
  }

  eq(other: CheckboxWidget): boolean {
    return other.checked === this.checked && other.from === this.from && other.disabled === this.disabled;
  }

  toDOM(): HTMLElement {
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = this.checked;
    input.disabled = this.disabled;
    input.className = "inkflow-task-checkbox";
    input.setAttribute("aria-label", this.checked ? "Mark task incomplete" : "Mark task complete");
    input.addEventListener("mousedown", (event) => event.preventDefault());
    input.addEventListener("change", () => this.toggle(this.from, input.checked));
    return input;
  }

  ignoreEvent(): boolean {
    return false;
  }
}

class ImageWidget extends WidgetType {
  constructor(
    readonly documentId: string,
    readonly source: string,
    readonly alt: string,
    readonly loader: FusionOptions["loadResource"],
    readonly allowRemote: boolean,
  ) {
    super();
  }

  eq(other: ImageWidget): boolean {
    return other.documentId === this.documentId && other.source === this.source && other.alt === this.alt && other.allowRemote === this.allowRemote;
  }

  toDOM(): HTMLElement {
    const figure = document.createElement("span");
    figure.className = "inkflow-inline-image";
    figure.textContent = "Loading image…";
    const source = decodeMarkdownResourceDestination(this.source);
    const remote = isRemoteImageSource(source);
    const load = remote
      ? this.allowRemote
        ? Promise.resolve(source)
        : Promise.reject(new Error("Remote image blocked"))
      : this.loader(this.documentId, source);
    void load
      .then((source) => {
        const image = document.createElement("img");
        image.src = source;
        image.alt = this.alt;
        image.loading = "lazy";
        figure.replaceChildren(image);
      })
      .catch(() => {
        figure.textContent = `Image: ${this.alt || this.source}`;
        figure.classList.add("is-blocked");
      });
    return figure;
  }
}

class MathWidget extends WidgetType {
  constructor(readonly source: string) {
    super();
  }

  eq(other: MathWidget): boolean {
    return other.source === this.source;
  }

  toDOM(): HTMLElement {
    const span = document.createElement("span");
    span.className = "inkflow-inline-math";
    span.textContent = `$${this.source}$`;
    void renderMath(this.source, span, false);
    return span;
  }
}

type TableAction = "add-row" | "remove-row" | "add-column" | "remove-column";

class TableWidget extends WidgetType {
  constructor(
    readonly source: string,
    readonly position: number,
    readonly readOnly: boolean,
    readonly reveal: () => void,
    readonly edit: (action: TableAction) => void,
  ) {
    super();
  }

  eq(other: TableWidget): boolean {
    return other.source === this.source
      && other.position === this.position
      && other.readOnly === this.readOnly;
  }

  toDOM(): HTMLElement {
    const wrapper = document.createElement("div");
    wrapper.className = "inkflow-table-widget";
    const rows = this.source.split("\n").map(splitTableRow);
    const table = document.createElement("table");
    const head = document.createElement("thead");
    const body = document.createElement("tbody");
    appendTableRow(head, rows[0] ?? [], "th");
    for (const row of rows.slice(2)) appendTableRow(body, row, "td");
    table.append(head, body);
    table.addEventListener("click", this.reveal);
    wrapper.append(table);

    if (!this.readOnly) {
      const toolbar = document.createElement("div");
      toolbar.className = "inkflow-table-tools";
      for (const [label, action] of [
        ["+ 行", "add-row"],
        ["− 行", "remove-row"],
        ["+ 列", "add-column"],
        ["− 列", "remove-column"],
      ] as const) {
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = label;
        button.addEventListener("mousedown", (event) => event.preventDefault());
        button.addEventListener("click", (event) => {
          event.stopPropagation();
          this.edit(action);
        });
        toolbar.append(button);
      }
      wrapper.append(toolbar);
    }
    return wrapper;
  }

  ignoreEvent(): boolean {
    return false;
  }
}

class RenderedBlockWidget extends WidgetType {
  private destroyed = false;

  constructor(
    readonly kind: "code" | "math" | "mermaid",
    readonly source: string,
    readonly language: string,
    readonly position: number,
    readonly reveal: () => void,
    readonly documentId: string,
    readonly loader: FusionOptions["loadResource"],
    readonly allowRemoteImages: boolean,
  ) {
    super();
  }

  eq(other: RenderedBlockWidget): boolean {
    return other.kind === this.kind
      && other.source === this.source
      && other.language === this.language
      && other.position === this.position
      && other.documentId === this.documentId
      && other.allowRemoteImages === this.allowRemoteImages;
  }

  toDOM(): HTMLElement {
    const wrapper = document.createElement("div");
    wrapper.className = `inkflow-block-widget inkflow-block-${this.kind}`;
    wrapper.addEventListener("click", this.reveal);
    if (this.kind === "math") {
      wrapper.textContent = `$$\n${this.source}\n$$`;
      void renderMath(this.source, wrapper, true);
      return wrapper;
    }
    if (this.kind === "mermaid") {
      wrapper.textContent = "Rendering diagram…";
      void (async () => {
        const remoteImageBlocked = !this.allowRemoteImages
          && await hasRemoteMermaidImageReference(this.source);
        if (this.destroyed) return;
        if (remoteImageBlocked) {
          const pre = document.createElement("pre");
          pre.textContent = this.source;
          wrapper.replaceChildren(pre);
          wrapper.classList.add("is-error");
          return;
        }
        const source = await resolveLocalMermaidImageReferences(
          this.source,
          (resource) => this.loader(this.documentId, resource),
        );
        if (this.destroyed) return;
        const result = await renderMermaid(
          source,
          { startOnLoad: false, securityLevel: "strict", theme: "neutral" },
          "inkflow-live",
          () => !this.destroyed,
        );
        if (this.destroyed) return;
        wrapper.innerHTML = this.allowRemoteImages
          ? result.svg
          : blockRemoteImageRequests(result.svg);
      })()
        .catch(() => {
          if (this.destroyed) return;
          const pre = document.createElement("pre");
          pre.textContent = this.source;
          wrapper.replaceChildren(pre);
          wrapper.classList.add("is-error");
        });
      return wrapper;
    }
    const pre = document.createElement("pre");
    const code = document.createElement("code");
    code.textContent = this.source;
    if (this.language) code.dataset.language = this.language;
    pre.append(code);
    wrapper.append(pre);
    return wrapper;
  }

  destroy(): void {
    this.destroyed = true;
  }
}

export interface FusionBlock {
  from: number;
  to: number;
  kind: "table" | "code" | "math" | "mermaid";
  source: string;
  sourceLength: number;
  language: string;
}

export function transformMarkdownTable(source: string, action: TableAction): string {
  const rows = source.split("\n").map(splitTableRow);
  if (rows.length < 2 || rows[0].length === 0) return source;
  const columns = rows[0].length;
  if (action === "add-row") rows.push(Array(columns).fill(""));
  if (action === "remove-row" && rows.length > 2) rows.pop();
  if (action === "add-column") {
    rows.forEach((row, index) => row.push(index === 1 ? "---" : ""));
  }
  if (action === "remove-column" && columns > 1) rows.forEach((row) => row.pop());
  return rows.map((row) => `| ${row.join(" | ")} |`).join("\n");
}

function splitTableRow(row: string): string[] {
  return row.trim().replace(/^\|/, "").replace(/\|$/, "").split(/(?<!\\)\|/).map((cell) => cell.trim());
}

function appendTableRow(parent: HTMLElement, cells: string[], tag: "th" | "td"): void {
  const row = document.createElement("tr");
  for (const value of cells) {
    const cell = document.createElement(tag);
    cell.textContent = value.replace(/\\\|/g, "|");
    row.append(cell);
  }
  parent.append(row);
}

export function fusionExtension(options: FusionOptions): Extension {
  const setBlockDecorations = StateEffect.define<DecorationSet>();
  const blockField = StateField.define<DecorationSet>({
    create: () => Decoration.none,
    update(value, transaction) {
      if (transaction.docChanged || !transaction.startState.selection.eq(transaction.state.selection)) {
        value = Decoration.none;
      } else {
        value = value.map(transaction.changes);
      }
      for (const effect of transaction.effects) {
        if (effect.is(setBlockDecorations)) value = effect.value;
      }
      return value;
    },
    provide: (field) => EditorView.decorations.from(field),
  });
  const measureKey = {};
  const activeViews = new WeakSet<EditorView>();
  const scheduledTokens = new WeakMap<EditorView, object>();
  const scheduleBlocks = (view: EditorView) => {
    const token = {};
    scheduledTokens.set(view, token);
    view.requestMeasure({
      key: measureKey,
      read: () => ({ decorations: buildBlockDecorations(view, options), state: view.state, token }),
      write: (result) => queueMicrotask(() => {
        if (!activeViews.has(view) || scheduledTokens.get(view) !== result.token) return;
        if (view.state !== result.state) {
          scheduleBlocks(view);
          return;
        }
        view.dispatch({ effects: setBlockDecorations.of(result.decorations) });
      }),
    });
  };
  const blockPlugin = ViewPlugin.fromClass(
    class {
      readonly view: EditorView;

      constructor(view: EditorView) {
        this.view = view;
        activeViews.add(view);
        scheduleBlocks(view);
      }

      update(update: ViewUpdate) {
        if (update.view.composing) return;
        if (
          update.docChanged
          || update.selectionSet
          || update.viewportChanged
          || update.geometryChanged
          || update.startState.readOnly !== update.state.readOnly
        ) {
          scheduleBlocks(update.view);
        }
      }

      destroy() {
        activeViews.delete(this.view);
        scheduledTokens.delete(this.view);
      }
    },
  );
  const inlinePlugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;

      constructor(view: EditorView) {
        this.decorations = buildInlineDecorations(view, options);
      }

      update(update: ViewUpdate) {
        if (update.view.composing) return;
        if (
          update.docChanged
          || update.selectionSet
          || update.viewportChanged
          || update.startState.readOnly !== update.state.readOnly
        ) {
          this.decorations = buildInlineDecorations(update.view, options);
        }
      }
    },
    { decorations: (instance) => instance.decorations },
  );
  return [blockField, blockPlugin, inlinePlugin, fusionTheme];
}

function activeLineRanges(view: EditorView): { from: number; to: number }[] {
  return view.state.selection.ranges.map((range) => {
    const from = view.state.doc.lineAt(range.from);
    const to = view.state.doc.lineAt(range.to);
    return { from: from.from, to: to.to };
  });
}

function buildBlockDecorations(view: EditorView, options: FusionOptions): DecorationSet {
  const activeLines = activeLineRanges(view);
  const isActive = (from: number, to: number) =>
    activeLines.some((active) => from <= active.to && to >= active.from);
  const ranges = collectViewportBlocks(view)
    .filter((block) => block.sourceLength <= MAX_RENDERED_BLOCK_CHARS && !isActive(block.from, block.to))
    .map((block) => {
      const reveal = () => {
        view.dispatch({ selection: { anchor: block.from }, scrollIntoView: true });
        view.focus();
      };
      const widget = block.kind === "table"
        ? new TableWidget(block.source, block.from, view.state.readOnly, reveal, (action) => {
          if (view.state.readOnly) return;
          const replacement = transformMarkdownTable(block.source, action);
          view.dispatch({
            changes: { from: block.from, to: block.to, insert: replacement },
            selection: { anchor: block.from },
            userEvent: "input.table-command",
          });
          view.focus();
        })
        : new RenderedBlockWidget(
          block.kind,
          block.source,
          block.language,
          block.from,
          reveal,
          options.documentId,
          options.loadResource,
          options.allowRemoteImages,
        );
      return Decoration.replace({ widget, block: true }).range(block.from, block.to);
    });
  return Decoration.set(ranges, true);
}

function buildInlineDecorations(view: EditorView, options: FusionOptions): DecorationSet {
  const ranges: { from: number; to: number; value: Decoration }[] = [];
  const activeLines = activeLineRanges(view);
  const isActive = (from: number, to: number) =>
    activeLines.some((active) => from <= active.to && to >= active.from);
  const blocks = collectViewportBlocks(view);
  const covered = blocks.filter((block) => !isActive(block.from, block.to));

  for (const visible of view.visibleRanges) {
    syntaxTree(view.state).iterate({
      from: visible.from,
      to: visible.to,
      enter(node) {
        if (covered.some((block) => node.from >= block.from && node.to <= block.to)) return false;
        const heading = /^ATXHeading([1-6])$/.exec(node.name);
        if (heading) {
          const line = view.state.doc.lineAt(node.from);
          ranges.push({
            from: line.from,
            to: line.from,
            value: Decoration.line({ class: `inkflow-heading inkflow-h${heading[1]}` }),
          });
        }
        if (node.name === "StrongEmphasis") {
          ranges.push({ from: node.from, to: node.to, value: Decoration.mark({ class: "inkflow-strong" }) });
        }
        if (node.name === "Emphasis") {
          ranges.push({ from: node.from, to: node.to, value: Decoration.mark({ class: "inkflow-emphasis" }) });
        }
        if (node.name === "InlineCode") {
          ranges.push({ from: node.from, to: node.to, value: Decoration.mark({ class: "inkflow-code" }) });
        }
        if (
          !isActive(node.from, node.to) &&
          ["HeaderMark", "EmphasisMark", "CodeMark"].includes(node.name)
        ) {
          ranges.push({ from: node.from, to: node.to, value: Decoration.replace({}) });
        }
      },
    });

    let position = view.state.doc.lineAt(visible.from).from;
    while (position <= visible.to && position <= view.state.doc.length) {
      const line = view.state.doc.lineAt(position);
      const hiddenByBlock = blocks.some((block) => line.from <= block.to && line.to >= block.from);
      if (!hiddenByBlock && !isActive(line.from, line.to)) {
        const task = /^(\s*[-+*]\s+)\[([ xX])\]/.exec(line.text);
        if (task) {
          const bracketFrom = line.from + task[1].length;
          ranges.push({
            from: bracketFrom,
            to: bracketFrom + 3,
            value: Decoration.replace({
              widget: new CheckboxWidget(task[2].toLowerCase() === "x", bracketFrom, view.state.readOnly, (from, checked) => {
                if (view.state.readOnly) return;
                view.dispatch({ changes: { from, to: from + 3, insert: checked ? "[x]" : "[ ]" }, userEvent: "input.task" });
              }),
            }),
          });
        }

        for (const match of line.text.matchAll(/!\[([^\]]*)\]\((?:<([^>\r\n]+)>|([^\s)\r\n]+))(?:\s+["'][^)\r\n]*["'])?\)/g)) {
          if (match.index === undefined) continue;
          const from = line.from + match.index;
          const source = match[2] ?? match[3];
          ranges.push({
            from,
            to: from + match[0].length,
            value: Decoration.replace({
              widget: new ImageWidget(options.documentId, source, match[1], options.loadResource, options.allowRemoteImages),
            }),
          });
        }

        for (const match of line.text.matchAll(/(?<!\\)\$([^$\n]+)\$/g)) {
          if (match.index === undefined) continue;
          const from = line.from + match.index;
          ranges.push({
            from,
            to: from + match[0].length,
            value: Decoration.replace({ widget: new MathWidget(match[1]) }),
          });
        }
      }
      if (line.to >= view.state.doc.length) break;
      position = line.to + 1;
    }
  }
  return Decoration.set(ranges.map((range) => range.value.range(range.from, range.to)), true);
}

export function collectViewportBlocks(view: EditorView): FusionBlock[] {
  const result = collectFencedBlocks(view.state, view.visibleRanges);
  const seen = new Set(result.map((block) => block.from));
  const fencedBlocks = result;
  for (const visible of view.visibleRanges) {
    let position = view.state.doc.lineAt(visible.from).from;
    let inspected = 0;
    while (position <= visible.to && position <= view.state.doc.length && inspected < 1000) {
      inspected += 1;
      const line = view.state.doc.lineAt(position);
      const fenced = fencedBlocks.find((block) => line.from >= block.from && line.from <= block.to);
      if (fenced) {
        position = fenced.to < view.state.doc.length ? fenced.to + 1 : view.state.doc.length + 1;
        continue;
      }
      const fence = /^\s*(`{3,}|~{3,})\s*([^\s`]*)\s*$/.exec(line.text);
      if (fence && !seen.has(line.from)) {
        const closePattern = new RegExp(`^\\s*${fence[1][0]}{${fence[1].length},}\\s*$`);
        let cursor = line.to < view.state.doc.length ? line.to + 1 : view.state.doc.length;
        let closing = null as ReturnType<typeof view.state.doc.lineAt> | null;
        let lines = 0;
        let scanLimitReached = false;
        while (cursor <= view.state.doc.length && lines < MAX_FALLBACK_FENCE_LINES) {
          const candidate = view.state.doc.lineAt(cursor);
          if (candidate.to - line.to > MAX_RENDERED_BLOCK_CHARS) {
            scanLimitReached = true;
            break;
          }
          if (closePattern.test(candidate.text)) {
            closing = candidate;
            break;
          }
          if (candidate.to >= view.state.doc.length) break;
          cursor = candidate.to + 1;
          lines += 1;
        }
        if (!closing && lines >= MAX_FALLBACK_FENCE_LINES) scanLimitReached = true;
        if (closing) {
          const language = fence[2].toLowerCase();
          const sourceFrom = line.to < view.state.doc.length ? line.to + 1 : line.to;
          const sourceTo = Math.max(sourceFrom, closing.from - 1);
          const block: FusionBlock = {
            from: line.from,
            to: closing.to,
            kind: language === "mermaid" ? "mermaid" : "code",
            ...renderableBlockSource(view.state, sourceFrom, sourceTo),
            language,
          };
          result.push(block);
          seen.add(line.from);
          position = closing.to < view.state.doc.length ? closing.to + 1 : view.state.doc.length + 1;
          continue;
        }
        if (scanLimitReached) {
          // The syntax tree will supply the range once parsing catches up; do not
          // synchronously rescan a long unfinished block on every editor update.
          position = visible.to < view.state.doc.length
            ? visible.to + 1
            : view.state.doc.length + 1;
          continue;
        }
      }

      if (/^\s*\$\$\s*$/.test(line.text)) {
        let cursor = line.to < view.state.doc.length
          ? line.to + 1
          : view.state.doc.length + 1;
        let closing = null as ReturnType<typeof view.state.doc.lineAt> | null;
        let lines = 0;
        let scanLimitReached = false;
        while (cursor <= view.state.doc.length && lines < MAX_FALLBACK_MATH_LINES) {
          const candidate = view.state.doc.lineAt(cursor);
          if (candidate.to - line.to > MAX_RENDERED_BLOCK_CHARS) {
            scanLimitReached = true;
            break;
          }
          if (/^\s*\$\$\s*$/.test(candidate.text)) {
            closing = candidate;
            break;
          }
          if (candidate.to >= view.state.doc.length) break;
          cursor = candidate.to + 1;
          lines += 1;
        }
        if (!closing && lines >= MAX_FALLBACK_MATH_LINES) scanLimitReached = true;
        if (closing && !seen.has(line.from)) {
          const sourceFrom = line.to < view.state.doc.length ? line.to + 1 : line.to;
          const sourceTo = Math.max(sourceFrom, closing.from - 1);
          result.push({
            from: line.from,
            to: closing.to,
            kind: "math",
            ...renderableBlockSource(view.state, sourceFrom, sourceTo),
            language: "math",
          });
          seen.add(line.from);
          position = closing.to < view.state.doc.length ? closing.to + 1 : view.state.doc.length + 1;
          continue;
        }
        if (scanLimitReached) {
          position = visible.to < view.state.doc.length
            ? visible.to + 1
            : view.state.doc.length + 1;
          continue;
        }
      }

      if (line.to < view.state.doc.length) {
        const separator = view.state.doc.lineAt(line.to + 1);
        const headers = splitTableRow(line.text);
        const dividers = splitTableRow(separator.text);
        const isTable = line.text.includes("|")
          && headers.length > 0
          && headers.length === dividers.length
          && dividers.every((cell) => /^:?-{3,}:?$/.test(cell));
        if (isTable && !seen.has(line.from)) {
          let end = separator;
          let cursor = separator.to < view.state.doc.length ? separator.to + 1 : view.state.doc.length + 1;
          let rows = 0;
          let scanLimitReached = end.to - line.from > MAX_RENDERED_BLOCK_CHARS;
          while (cursor <= view.state.doc.length && !scanLimitReached) {
            const row = view.state.doc.lineAt(cursor);
            if (row.to - line.from > MAX_RENDERED_BLOCK_CHARS) {
              scanLimitReached = true;
              break;
            }
            if (!row.text.includes("|") || row.text.trim() === "") break;
            end = row;
            rows += 1;
            if (rows >= MAX_TABLE_SCAN_LINES) {
              scanLimitReached = true;
              break;
            }
            if (row.to >= view.state.doc.length) break;
            cursor = row.to + 1;
          }
          if (scanLimitReached) {
            // Keep oversized tables as source instead of walking the entire block
            // synchronously on every viewport or document update.
            position = visible.to < view.state.doc.length
              ? visible.to + 1
              : view.state.doc.length + 1;
            continue;
          }
          result.push({
            from: line.from,
            to: end.to,
            kind: "table",
            ...renderableBlockSource(view.state, line.from, end.to),
            language: "",
          });
          seen.add(line.from);
          position = end.to < view.state.doc.length ? end.to + 1 : view.state.doc.length + 1;
          continue;
        }
      }

      if (line.to >= view.state.doc.length) break;
      position = line.to + 1;
    }
  }
  return result;
}

export function collectFencedBlocks(
  state: EditorState,
  visibleRanges: readonly { from: number; to: number }[],
): FusionBlock[] {
  const result: FusionBlock[] = [];
  const seen = new Set<number>();
  const tree = syntaxTree(state);
  for (const visible of visibleRanges) {
    tree.iterate({
      from: visible.from,
      to: visible.to,
      enter(node) {
        if (node.name !== "FencedCode" || seen.has(node.from)) return;
        const opening = state.doc.lineAt(node.from);
        const fence = /^\s*(`{3,}|~{3,})\s*([^\s`]*)/.exec(opening.text);
        if (!fence || node.to <= opening.to) return;
        const closing = state.doc.lineAt(Math.max(node.from, node.to - 1));
        const closePattern = new RegExp(`^\\s*${fence[1][0]}{${fence[1].length},}\\s*$`);
        if (closing.from === opening.from || !closePattern.test(closing.text)) return;
        const sourceFrom = opening.to < state.doc.length ? opening.to + 1 : opening.to;
        const sourceTo = Math.max(sourceFrom, closing.from - 1);
        const language = fence[2].toLowerCase();
        result.push({
          from: opening.from,
          to: closing.to,
          kind: language === "mermaid" ? "mermaid" : "code",
          ...renderableBlockSource(state, sourceFrom, sourceTo),
          language,
        });
        seen.add(opening.from);
      },
    });
  }
  return result;
}

function renderableBlockSource(
  state: EditorState,
  from: number,
  to: number,
): Pick<FusionBlock, "source" | "sourceLength"> {
  const sourceLength = Math.max(0, to - from);
  return {
    source: sourceLength <= MAX_RENDERED_BLOCK_CHARS ? state.sliceDoc(from, to) : "",
    sourceLength,
  };
}

const fusionTheme = EditorView.baseTheme({
  ".inkflow-heading": { fontWeight: "650", lineHeight: "1.35" },
  ".inkflow-h1": { fontSize: "2em", marginTop: "0.75em" },
  ".inkflow-h2": { fontSize: "1.55em", marginTop: "0.7em" },
  ".inkflow-h3": { fontSize: "1.3em", marginTop: "0.55em" },
  ".inkflow-h4": { fontSize: "1.15em" },
  ".inkflow-h5": { fontSize: "1.05em" },
  ".inkflow-h6": { fontSize: "1em", color: "var(--muted)" },
  ".inkflow-strong": { fontWeight: "700" },
  ".inkflow-emphasis": { fontStyle: "italic" },
  ".inkflow-code": { fontFamily: "var(--code-font)", background: "var(--code-bg)", borderRadius: "4px" },
  ".inkflow-task-checkbox": { margin: "0 .45em 0 0", accentColor: "var(--accent)" },
  ".inkflow-inline-image": { display: "block", margin: "1em auto", textAlign: "center", color: "var(--muted)" },
  ".inkflow-inline-image img": { maxWidth: "100%", maxHeight: "70vh", borderRadius: "8px" },
  ".inkflow-inline-image.is-blocked": { padding: ".8em", border: "1px dashed var(--line)", borderRadius: "8px" },
  ".inkflow-inline-math": { padding: "0 .12em" },
  ".inkflow-table-widget": { position: "relative", width: "min(100%, 820px)", margin: "1em auto", overflowX: "auto" },
  ".inkflow-table-widget table": { width: "100%", borderCollapse: "collapse", cursor: "text" },
  ".inkflow-table-widget th,.inkflow-table-widget td": { padding: ".45em .7em", border: "1px solid var(--line)", textAlign: "left" },
  ".inkflow-table-widget th": { background: "var(--subtle)", fontWeight: "650" },
  ".inkflow-table-tools": { position: "absolute", top: "-12px", right: "6px", display: "none", gap: "3px", padding: "3px", border: "1px solid var(--line)", borderRadius: "6px", background: "var(--panel)", boxShadow: "var(--shadow-lg)" },
  ".inkflow-table-widget:hover .inkflow-table-tools": { display: "flex" },
  ".inkflow-table-tools button": { padding: "3px 6px", border: "0", borderRadius: "4px", background: "transparent", color: "var(--muted)", cursor: "pointer" },
  ".inkflow-table-tools button:hover": { background: "var(--hover)", color: "var(--ink)" },
  ".inkflow-block-widget": { width: "min(100%, 820px)", margin: "1em auto", padding: ".8em 1em", overflowX: "auto", border: "1px solid var(--line)", borderRadius: "8px", background: "var(--code-block)", cursor: "text" },
  ".inkflow-block-widget pre": { margin: "0", whiteSpace: "pre-wrap" },
  ".inkflow-block-widget code": { fontFamily: "var(--code-font)" },
  ".inkflow-block-math,.inkflow-block-mermaid": { background: "transparent", textAlign: "center" },
  ".inkflow-block-mermaid svg": { maxWidth: "100%", height: "auto" },
  ".inkflow-block-widget.is-error": { color: "var(--danger)", textAlign: "left" },
});
