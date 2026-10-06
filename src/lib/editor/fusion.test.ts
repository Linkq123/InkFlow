import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { forceParsing, ParseContext, syntaxTreeAvailable } from "@codemirror/language";
import { markdown as markdownSupport, markdownLanguage } from "@codemirror/lang-markdown";
import { afterEach, describe, expect, it, vi } from "vitest";
import { collectFencedBlocks, collectViewportBlocks, fusionExtension, transformMarkdownTable } from "./fusion";
import { renderMarkdown } from "../markdown/pipeline";

const mocks = vi.hoisted(() => ({
  detectRemoteMermaidImage: vi.fn(async () => false),
  mermaidInitialize: vi.fn(),
  mermaidRender: vi.fn(async (_id: string, _source: string) => ({ svg: "<svg></svg>" })),
}));

vi.mock("../markdown/resources", async (importOriginal) => ({
  ...await importOriginal<typeof import("../markdown/resources")>(),
  hasRemoteMermaidImageReference: mocks.detectRemoteMermaidImage,
}));

vi.mock("../markdown/mermaid-service", () => ({
  renderMermaid: (source: string, config: unknown, idPrefix: string) => {
    mocks.mermaidInitialize(config);
    return mocks.mermaidRender(`${idPrefix}-test`, source);
  },
}));

afterEach(() => {
  vi.clearAllMocks();
});

const table = [
  "| Name | Ready |",
  "| --- | :---: |",
  "| InkFlow | yes |",
].join("\n");

const markdown = () => markdownSupport({ base: markdownLanguage });

describe("Markdown table commands", () => {
  it.each(["add-row", "remove-row", "add-column", "remove-column"] as const)("keeps a list table and following paragraph nested after %s", async action => {
    const source = "- item\n\n  | A | B |\n  | --- | --- |\n  | x | y |\n\n  tail";
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: source, extensions: [markdown()] }) });
    try {
      const block = collectViewportBlocks(view).find(block => block.kind === "table")!;
      expect(block).toBeDefined();
      const edited = source.slice(0, block.from) + transformMarkdownTable(block.source, action) + source.slice(block.to);
      const html = new DOMParser().parseFromString(await renderMarkdown(edited), "text/html");
      expect(html.querySelector("li table")).not.toBeNull();
      expect(html.querySelector("li p:last-child")?.textContent).toBe("tail");
      expect(html.body.children).toHaveLength(1);
    } finally { view.destroy(); parent.remove(); }
  });
  it("adds and removes rows without changing the existing cells", () => {
    const added = transformMarkdownTable(table, "add-row");
    expect(added).toContain("| InkFlow | yes |");
    expect(added.split("\n")).toHaveLength(4);
    expect(transformMarkdownTable(added, "remove-row")).toBe(table);
  });

  it("adds and removes columns while keeping a valid separator", () => {
    const added = transformMarkdownTable(table, "add-column");
    expect(added.split("\n")[1]).toBe("| --- | :---: | --- |");
    expect(transformMarkdownTable(added, "remove-column")).toBe(table);
  });

  it("preserves escaped pipes inside cells", () => {
    const escaped = "| Value | State |\n| --- | --- |\n| a\\|b | ready |";
    expect(transformMarkdownTable(escaped, "add-row")).toContain("| a\\|b | ready |");
  });
});

describe("live fusion blocks", () => {
  it("keeps task markers from an embedded Markdown code language non-interactive", () => {
    const source = "```markdown\ncursor\n\n- [ ] literal\n```";
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: source,
      selection: { anchor: source.indexOf("cursor") },
      extensions: [
        markdownSupport({ base: markdownLanguage, codeLanguages: () => markdownLanguage }),
        fusionExtension({ documentId: "embedded-code", allowRemoteImages: false, loadResource: async () => "" }),
      ],
    }) });
    try {
      expect(parent.querySelector(".inkflow-task-checkbox")).toBeNull();
      expect(view.state.doc.toString()).toBe(source);
    } finally { view.destroy(); parent.remove(); }
  });

  it.each(["cursor\n\n    - [ ] literal", "cursor\n\n```markdown\n- [ ] literal", "cursor\n\n<div>\n- [ ] literal\n</div>"])("keeps task-like source opaque in code or HTML: %s", async source => {
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: source,
      extensions: [markdown(), fusionExtension({ documentId: "opaque", allowRemoteImages: false, loadResource: async () => "" })],
    }) });
    try {
      expect(parent.querySelector(".inkflow-task-checkbox")).toBeNull();
      expect(view.state.doc.toString()).toBe(source);
    } finally { view.destroy(); parent.remove(); }
  });

  it.each(["- [ ] task", "1. [ ] task", "> - [ ] task"])("toggles a parser-confirmed task: %s", async task => {
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: "cursor\n\n" + task,
      extensions: [markdown(), fusionExtension({ documentId: "task", allowRemoteImages: false, loadResource: async () => "" })],
    }) });
    try {
      const checkbox = parent.querySelector<HTMLInputElement>(".inkflow-task-checkbox");
      expect(checkbox).not.toBeNull();
      checkbox!.click();
      expect(view.state.doc.toString()).toContain(task.replace("[ ]", "[x]"));
    } finally { view.destroy(); parent.remove(); }
  });
  it("maps hidden markers during IME input and still reports document changes", async () => {
    const parent = document.createElement("div"); document.body.append(parent);
    const changed = vi.fn();
    const view = new EditorView({ parent, state: EditorState.create({
      doc: "cursor\n\n**bold**", selection: { anchor: 0 },
      extensions: [markdown(), fusionExtension({ documentId: "ime", allowRemoteImages: false, loadResource: async () => "" }),
        EditorView.updateListener.of(update => { if (update.docChanged) changed(update.state.doc.toString()); })],
    }) });
    try {
      Object.defineProperty(view, "composing", { configurable: true, value: true });
      expect(() => view.dispatch({ changes: { from: 0, insert: "中" } })).not.toThrow();
      expect(changed).toHaveBeenCalledExactlyOnceWith("中cursor\n\n**bold**");
      Reflect.deleteProperty(view, "composing");
      view.contentDOM.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true }));
      await Promise.resolve();
      expect(view.contentDOM.textContent).toContain("bold");
      expect(view.contentDOM.textContent).not.toContain("**");
    } finally { Reflect.deleteProperty(view, "composing"); view.destroy(); parent.remove(); }
  });

  it.each(["add-row", "remove-row", "add-column", "remove-column"] as const)("keeps escaped trailing pipes when a borderless table uses %s", async action => {
    const source = "A | B | C\n--- | --- | ---\na | b\\|\nz | last | cell";
    const rewritten = transformMarkdownTable(source, action);
    const doc = new DOMParser().parseFromString(await renderMarkdown(rewritten), "text/html");
    expect(doc.querySelectorAll("tbody tr")[0].querySelectorAll("td")[1].textContent).toBe("b|");
  });

  it("renders large Mermaid images with short layout placeholders", async () => {
    const embedded = `data:image/png;base64,${"A".repeat(54000)}`;
    vi.stubGlobal("Image", class { src = ""; naturalWidth = 100; naturalHeight = 100; async decode() {} });
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:layout");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    mocks.mermaidRender.mockImplementationOnce(async (_id: string, source: string) => {
      expect(source.length).toBeLessThan(50000);
      const image = /data:image\/svg\+xml;base64,[A-Za-z0-9+/=]+/.exec(source)![0];
      return { svg: `<svg><image href="${image}"></image></svg>` };
    });
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: 'cursor\n\n```mermaid\nflowchart LR\nA@{ img: "logo.png" }\n```',
      extensions: [markdown(), fusionExtension({ documentId: "diagram", allowRemoteImages: false, loadResource: async () => embedded })],
    }) });
    try {
      await vi.waitFor(() => expect(parent.querySelector("svg image")?.getAttribute("href")).toBe(embedded));
    } finally { view.destroy(); parent.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals(); }
  });
  it.each([1, 2, 3])("renders and edits a table with %i leading spaces without touching the next heading", async indent => {
    const parent = document.createElement("div");
    document.body.append(parent);
    const source = `cursor\n\n${table.split("\n").map(line => " ".repeat(indent) + line).join("\n")}\n# Keep | heading`;
    const view = new EditorView({ parent, state: EditorState.create({ doc: source,
      extensions: [markdown(), fusionExtension({ documentId: "test", allowRemoteImages: false, loadResource: async () => "" })],
    }) });
    try {
      await vi.waitFor(() => expect(parent.querySelector(".inkflow-table-tools")).not.toBeNull());
      const blocks = collectViewportBlocks(view).filter(block => block.kind === "table");
      expect(blocks).toHaveLength(1);
      expect(source.slice(blocks[0].from, blocks[0].to)).not.toContain("Keep");
      [...parent.querySelectorAll<HTMLButtonElement>(".inkflow-table-tools button")]
        .find(button => button.textContent === "− 行")!.click();
      expect(view.state.doc.toString()).toBe(`cursor\n\n${" ".repeat(indent)}| Name | Ready |\n${" ".repeat(indent)}| --- | :---: |\n# Keep | heading`);
    } finally { view.destroy(); parent.remove(); }
  });

  it("keeps a four-space-indented table example as code", () => {
    const parent = document.createElement("div");
    document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({
      doc: `cursor\n\n${table.split("\n").map(line => "    " + line).join("\n")}`,
      extensions: [markdown(), fusionExtension({ documentId: "test", allowRemoteImages: false, loadResource: async () => "" })],
    }) });
    try {
      expect(collectViewportBlocks(view).filter(block => block.kind === "table")).toHaveLength(0);
      expect(parent.querySelector(".inkflow-table-tools")).toBeNull();
    } finally { view.destroy(); parent.remove(); }
  });

  it("removes the header's last column without deleting a short row's first cell", async () => {
    const parent = document.createElement("div");
    document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({
      doc: "cursor\n\n| A | B |\n| --- | --- |\n| important |",
      extensions: [markdown(), fusionExtension({ documentId: "test", allowRemoteImages: false, loadResource: async () => "" })],
    }) });
    try {
      await vi.waitFor(() => expect(parent.querySelector(".inkflow-table-tools")).not.toBeNull());
      const button = [...parent.querySelectorAll<HTMLButtonElement>(".inkflow-table-tools button")]
        .find(button => button.textContent === "− 列")!;
      button.click();
      expect(view.state.doc.toString()).toBe("cursor\n\n| A |\n| --- |\n| important |");
    } finally { view.destroy(); parent.remove(); }
  });

  it.each(["# Keep | heading", "- Keep | list", "1. Keep | ordered list", "> Keep | quote"])(
    "preserves the independent block after a table: %s", async following => {
      const parent = document.createElement("div");
      document.body.append(parent);
      const source = `cursor\n\n| H | V |\n| --- | --- |\n| a | b |\n${following}`;
      const html = await renderMarkdown(source);
      expect(html.indexOf("</table>")).toBeLessThan(html.indexOf("Keep"));
      const view = new EditorView({ parent, state: EditorState.create({ doc: source,
        extensions: [markdown(), fusionExtension({ documentId: "test", allowRemoteImages: false, loadResource: async () => "" })],
      }) });
      try {
        await vi.waitFor(() => expect(parent.querySelector(".inkflow-table-tools")).not.toBeNull());
        const button = [...parent.querySelectorAll<HTMLButtonElement>(".inkflow-table-tools button")]
          .find(button => button.textContent === "− 行")!;
        button.click();
        expect(view.state.doc.toString()).toBe(`cursor\n\n| H | V |\n| --- | --- |\n${following}`);
      } finally { view.destroy(); parent.remove(); }
    },
  );

  it("replaces inactive tables, display math, and Mermaid fences", async () => {
    const source = [
      "# InkFlow",
      "",
      "cursor line",
      "",
      "| Feature | State |",
      "| --- | --- |",
      "| Table | Ready |",
      "",
      "$$",
      "x^2 + y^2 = z^2",
      "$$",
      "",
      "```mermaid",
      "graph LR",
      "  A --> B",
      "```",
    ].join("\n");
    const parent = document.createElement("div");
    document.body.append(parent);
    const state = EditorState.create({
      doc: source,
      selection: { anchor: source.indexOf("cursor line") },
      extensions: [
        markdown(),
        fusionExtension({
          documentId: "test",
          allowRemoteImages: false,
          loadResource: async () => "",
        }),
      ],
    });
    const view = new EditorView({ state, parent });
    await new Promise((resolve) => setTimeout(resolve, 50));

    expect(view.dom.querySelector(".inkflow-table-widget")).not.toBeNull();
    expect(view.dom.querySelector(".inkflow-block-math")).not.toBeNull();
    expect(view.dom.querySelector(".inkflow-block-mermaid")).not.toBeNull();

    view.destroy();
    parent.remove();
  });

  it("loads titled images with the decoded Markdown destination", async () => {
    const source = "cursor\n\n![diagram](assets/diagram&amp;notes.png \"Architecture\")";
    const loaded: Array<[string, string]> = [];
    const parent = document.createElement("div");
    document.body.append(parent);
    const state = EditorState.create({
      doc: source,
      selection: { anchor: 0 },
      extensions: [
        markdown(),
        fusionExtension({
          documentId: "document-b",
          allowRemoteImages: false,
          loadResource: async (documentId, resource) => {
            loaded.push([documentId, resource]);
            return "data:image/png;base64,aW1hZ2U=";
          },
        }),
      ],
    });
    const view = new EditorView({ state, parent });
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(loaded).toContainEqual(["document-b", "assets/diagram&notes.png"]);

    view.destroy();
    parent.remove();
  });

  it("loads local Mermaid img metadata without rewriting Iconify identifiers", async () => {
    const source = [
      "cursor",
      "",
      "```mermaid",
      "flowchart LR",
      'A@{ img: "assets/a.png", icon: "logos:github-icon" }',
      "```",
    ].join("\n");
    const loaded: Array<[string, string]> = [];
    const parent = document.createElement("div");
    document.body.append(parent);
    const state = EditorState.create({
      doc: source,
      selection: { anchor: 0 },
      extensions: [
        fusionExtension({
          documentId: "fusion-document",
          allowRemoteImages: false,
          loadResource: async (documentId, resource) => {
            loaded.push([documentId, resource]);
            return "data:image/png;base64,YQ==";
          },
        }),
      ],
    });
    const view = new EditorView({ state, parent });

    await vi.waitFor(() => expect(mocks.mermaidRender).toHaveBeenCalledOnce());
    expect(loaded).toContainEqual(["fusion-document", "assets/a.png"]);
    expect(loaded).toHaveLength(1);
    const renderedSource = mocks.mermaidRender.mock.calls[0][1] as string;
    expect(renderedSource).toContain("data:image/png;base64,YQ==");
    expect(renderedSource).toContain("logos:github-icon");

    view.destroy();
    parent.remove();
  });

  it("keeps Mermaid source inert when a scoped local image load fails", async () => {
    const source = [
      "cursor",
      "",
      "```mermaid",
      "flowchart LR",
      'A@{ img: "assets/missing.png" }',
      "```",
    ].join("\n");
    const parent = document.createElement("div");
    document.body.append(parent);
    const state = EditorState.create({
      doc: source,
      selection: { anchor: 0 },
      extensions: [
        fusionExtension({
          documentId: "fusion-document",
          allowRemoteImages: false,
          loadResource: async () => {
            throw new Error("resource scope rejected the path");
          },
        }),
      ],
    });
    const view = new EditorView({ state, parent });

    await vi.waitFor(() => {
      expect(parent.querySelector(".inkflow-block-mermaid")?.classList.contains("is-error"))
        .toBe(true);
    });
    expect(mocks.mermaidRender).not.toHaveBeenCalled();
    expect(parent.querySelector(".inkflow-block-mermaid pre")?.textContent)
      .toContain("assets/missing.png");

    view.destroy();
    parent.remove();
  });

  it("does not let task widgets modify a read-only document", async () => {
    const source = "cursor\n\n- [ ] locked";
    const parent = document.createElement("div");
    document.body.append(parent);
    const state = EditorState.create({
      doc: source,
      selection: { anchor: 0 },
      extensions: [
        EditorState.readOnly.of(true),
        markdown(),
        fusionExtension({
          documentId: "read-only",
          allowRemoteImages: false,
          loadResource: async () => "",
        }),
      ],
    });
    const view = new EditorView({ state, parent });
    await new Promise((resolve) => setTimeout(resolve, 20));
    const checkbox = view.dom.querySelector<HTMLInputElement>(".inkflow-task-checkbox");

    expect(checkbox?.disabled).toBe(true);
    if (checkbox) {
      checkbox.checked = true;
      checkbox.dispatchEvent(new Event("change"));
    }
    expect(view.state.doc.toString()).toBe(source);
    view.destroy();
    parent.remove();
  });

  it("cancels Mermaid hydration when its widget is destroyed", async () => {
    let finishDetection: (detected: boolean) => void = () => undefined;
    mocks.detectRemoteMermaidImage.mockImplementationOnce(() =>
      new Promise<boolean>((resolve) => {
        finishDetection = resolve;
      })
    );
    const source = [
      "cursor",
      "",
      "```mermaid",
      "flowchart LR",
      "A --> B",
      "```",
    ].join("\n");
    const parent = document.createElement("div");
    document.body.append(parent);
    const state = EditorState.create({
      doc: source,
      selection: { anchor: 0 },
      extensions: [
        fusionExtension({
          documentId: "cancelled",
          allowRemoteImages: false,
          loadResource: async () => "",
        }),
      ],
    });
    const view = new EditorView({ state, parent });
    await vi.waitFor(() => {
      expect(mocks.detectRemoteMermaidImage).toHaveBeenCalledOnce();
    });

    view.destroy();
    finishDetection(false);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(mocks.mermaidRender).not.toHaveBeenCalled();
    parent.remove();
  });

  it("finds a long fenced block when the viewport starts inside it", () => {
    const source = ["```text", ...Array.from({ length: 20_000 }, (_, index) => `line ${index}`), "```"].join("\n");
    const state = EditorState.create({ doc: source, extensions: [markdown()] });
    const target = source.indexOf("line 15_000");

    const blocks = collectFencedBlocks(state, [{ from: target, to: target + 8 }]);

    expect(blocks).toHaveLength(1);
    expect(blocks[0].from).toBe(0);
    expect(blocks[0].sourceLength).toBeGreaterThan(200_000);
    expect(blocks[0].source).toBe("");
  });

  it("bounds fallback scanning for a long unfinished fenced block", () => {
    const source = ["```text", ...Array.from({ length: 20_000 }, (_, index) => `line ${index}`)].join("\n");
    const parent = document.createElement("div");
    document.body.append(parent);
    const state = EditorState.create({ doc: source, extensions: [markdown()] });
    const view = new EditorView({ state, parent });
    const lineAt = vi.spyOn(view.state.doc, "lineAt");

    const blocks = collectViewportBlocks(view);

    expect(blocks).toHaveLength(0);
    expect(lineAt.mock.calls.length).toBeLessThan(2_000);
    lineAt.mockRestore();
    view.destroy();
    parent.remove();
  });

  it("stops display-math fallback scanning at the character budget", () => {
    const source = ["$$", "x".repeat(200_001), "$$"].join("\n");
    const parent = document.createElement("div");
    document.body.append(parent);
    const state = EditorState.create({ doc: source, extensions: [markdown()] });
    const view = new EditorView({ state, parent });
    const lineAt = vi.spyOn(view.state.doc, "lineAt");

    const blocks = collectViewportBlocks(view);

    expect(blocks).toHaveLength(0);
    expect(lineAt.mock.calls.length).toBeLessThan(20);
    lineAt.mockRestore();
    view.destroy();
    parent.remove();
  });

  it("bounds scanning for a table with many rows", () => {
    const source = [
      "| Value |",
      "| --- |",
      ...Array.from({ length: 20_000 }, (_, index) => `| row ${index} |`),
    ].join("\n");
    const parent = document.createElement("div");
    document.body.append(parent);
    const state = EditorState.create({ doc: source, extensions: [markdown()] });
    const view = new EditorView({ state, parent });
    const lineAt = vi.spyOn(view.state.doc, "lineAt");

    const blocks = collectViewportBlocks(view);

    expect(blocks).toHaveLength(0);
    expect(lineAt.mock.calls.length).toBeLessThan(2_000);
    lineAt.mockRestore();
    view.destroy();
    parent.remove();
  });
});

describe("fusion image and math syntax boundaries", () => {
  it.each(["- [ ]", "- [x]", "1. [ ]", "> - [ ]"])("renders inactive task math without interpreting inline code: %s", async marker => {
    const source = "cursor\n\n" + marker + " calculate $x+1$ and `$literal$`";
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: source,
      extensions: [markdown(), fusionExtension({ documentId: "task-math", allowRemoteImages: false, loadResource: async () => "" })],
    }) });
    try {
      expect(parent.querySelectorAll(".inkflow-inline-math")).toHaveLength(1);
      await vi.waitFor(() => expect(parent.querySelector(".inkflow-inline-math .katex")).not.toBeNull());
      expect(view.state.doc.toString()).toBe(source);
      view.dispatch({ selection: { anchor: source.indexOf("calculate") } });
      expect(parent.querySelector(".inkflow-inline-math")).toBeNull();
      expect(view.state.doc.toString()).toBe(source);
    } finally { view.destroy(); parent.remove(); }
  });
  it.each([
    "`![example](secret.png)`", "    ![example](secret.png)", "\\![example](secret.png)",
    "`$x$`", "    $x$", "`across\n![example](secret.png)\n$x$`",
  ])("keeps literal Markdown inert: %s", async example => {
    const source = `cursor\n\n${example}`;
    const parent = document.createElement("div");
    document.body.append(parent);
    const loadResource = vi.fn(async () => "data:image/png;base64,YQ==");
    const view = new EditorView({ parent, state: EditorState.create({ doc: source,
      extensions: [markdown(), fusionExtension({ documentId: "test", allowRemoteImages: false, loadResource })],
    }) });
    try {
      expect(await renderMarkdown(source)).not.toContain("<img");
      expect(parent.querySelector(".inkflow-inline-image, .inkflow-inline-math")).toBeNull();
      expect(loadResource).not.toHaveBeenCalled();
    } finally { view.destroy(); parent.remove(); }
  });

  it.each([
    [String.raw`![x](assets/a\&amp;.png)`, "assets/a&amp;.png", "x"],
    ["![example](assets/foo(bar).png)", "assets/foo(bar).png", "example"],
    ["![photo\\]](image.png)", "image.png", "photo]"],
    ["![photo\\[](image.png)", "image.png", "photo["],
    ["![example](<assets/a b.png>)", "assets/a b.png", "example"],
  ])("renders parsed image destinations: %s", async (image, destination, alt) => {
    const source = `cursor\n\n${image}`;
    const parent = document.createElement("div");
    document.body.append(parent);
    const loadResource = vi.fn(async () => "data:image/png;base64,YQ==");
    const view = new EditorView({ parent, state: EditorState.create({ doc: source,
      extensions: [markdown(), fusionExtension({ documentId: "test", allowRemoteImages: false, loadResource })],
    }) });
    try {
      expect(await renderMarkdown(source)).toContain("<img");
      await vi.waitFor(() => expect(loadResource).toHaveBeenCalledWith("test", destination));
      expect(parent.querySelector(".inkflow-inline-image img")?.getAttribute("alt")).toBe(alt);
    } finally { view.destroy(); parent.remove(); }
  });

  it("still renders math in ordinary inactive prose", () => {
    const parent = document.createElement("div");
    document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: "cursor\n\nValue $x$",
      extensions: [markdown(), fusionExtension({ documentId: "test", allowRemoteImages: false, loadResource: async () => "" })],
    }) });
    try { expect(parent.querySelector(".inkflow-inline-math")).not.toBeNull(); }
    finally { view.destroy(); parent.remove(); }
  });
});

describe("table syntax context", () => {
  it.each([
    "only first cell",
    "| first | value |\nonly first cell",
    "| first | value |\nonly first cell\n| last | value |",
  ])("previews and edits all rows when a table contains a row without pipes: %s", async body => {
    const sourceTable = "| A | B |\n| --- | --- |\n" + body;
    const source = "cursor\n\n" + sourceTable + "\n# Keep heading";
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: source, extensions: [
      markdown(),
      fusionExtension({ documentId: "table-short-row", allowRemoteImages: false, loadResource: async () => "" }),
    ] }) });
    try {
      await vi.waitFor(() => expect(parent.querySelector(".inkflow-table-tools")).not.toBeNull());
      expect(collectViewportBlocks(view).find(block => block.kind === "table")?.source).toBe(sourceTable);
      const previewRows = parent.querySelectorAll(".inkflow-table-widget tbody tr");
      expect(previewRows).toHaveLength(body.split("\n").length);
      expect([...previewRows].some(row => row.firstElementChild?.textContent === "only first cell")).toBe(true);
      const button = [...parent.querySelectorAll<HTMLButtonElement>(".inkflow-table-tools button")]
        .find(button => button.textContent === "+ 列")!;
      button.click();
      const edited = view.state.doc.toString();
      const html = new DOMParser().parseFromString(await renderMarkdown(edited), "text/html");
      expect(html.querySelectorAll("thead th")).toHaveLength(3);
      expect(html.querySelectorAll("tbody tr")).toHaveLength(previewRows.length);
      expect(html.querySelector("table")?.textContent).toContain("only first cell");
      expect(edited.endsWith("\n# Keep heading")).toBe(true);
    } finally { view.destroy(); parent.remove(); }
  });

  it("refreshes table tools when delayed parsing completes without another interaction", async () => {
    let ready = false;
    let finish!: () => void;
    const pending = new Promise<void>(resolve => { finish = resolve; });
    const source = "cursor\n\n" + table;
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({
      doc: source,
      extensions: [
        markdownSupport({ base: markdownLanguage, extensions: {
          wrap: (inner, input, fragments, ranges) => ready
            ? inner
            : ParseContext.getSkippingParser(pending).startParse(input, fragments, ranges),
        } }),
        fusionExtension({ documentId: "delayed-table", allowRemoteImages: false, loadResource: async () => "" }),
      ],
    }) });
    try {
      // Drain the initial decoration measurement while the table is unparsed.
      await new Promise<void>(resolve => view.requestMeasure({
        read: () => null, write: () => queueMicrotask(resolve),
      }));
      expect(syntaxTreeAvailable(view.state, source.length)).toBe(false);
      expect(parent.querySelector(".inkflow-table-tools")).toBeNull();
      const selection = view.state.selection;
      ready = true;
      finish();
      expect(forceParsing(view, source.length)).toBe(true);
      expect(collectViewportBlocks(view).some(block => block.kind === "table")).toBe(true);
      await vi.waitFor(() => expect(parent.querySelector(".inkflow-table-tools")).not.toBeNull());
      expect(view.state.selection.eq(selection)).toBe(true);
      expect(view.state.doc.toString()).toBe(source);
    } finally { ready = true; finish(); view.destroy(); parent.remove(); }
  });

  it.each([
    "cursor\n\n<!--\n| A | B |\n| --- | --- |\n| keep | text |\n-->",
    "cursor\n\n```markdown\n| A | B |\n| --- | --- |\n| keep | text |",
    "cursor\n\n```markdown\n| A | B |\n| --- | --- |\n| keep | text |\n```",
  ])("does not expose table mutations in opaque source: %s", async source => {
    expect(await renderMarkdown(source)).not.toContain("<table>");
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: source, extensions: [
      markdownSupport({ base: markdownLanguage, codeLanguages: () => markdownLanguage }),
      fusionExtension({ documentId: "opaque-table", allowRemoteImages: false, loadResource: async () => "" }),
    ] }) });
    try {
      expect(collectViewportBlocks(view).filter(block => block.kind === "table")).toHaveLength(0);
      expect(parent.querySelector(".inkflow-table-tools")).toBeNull();
      expect(view.state.doc.toString()).toBe(source);
    } finally { view.destroy(); parent.remove(); }
  });
  it.each(["-", "--", ":-:"])("edits a parser-confirmed table with separator %s", async separator => {
    const source = "cursor\n\n| A | B |\n| " + separator + " | --- |\n| keep | text |";
    expect(await renderMarkdown(source)).toContain("<table>");
    const parent = document.createElement("div"); document.body.append(parent);
    const view = new EditorView({ parent, state: EditorState.create({ doc: source, extensions: [
      markdownSupport({ base: markdownLanguage }),
      fusionExtension({ documentId: "short-table", allowRemoteImages: false, loadResource: async () => "" }),
    ] }) });
    try {
      await vi.waitFor(() => expect(parent.querySelector(".inkflow-table-tools")).not.toBeNull());
      const button = [...parent.querySelectorAll<HTMLButtonElement>(".inkflow-table-tools button")].find(button => button.textContent === "+ 列");
      expect(button).toBeDefined();
      button!.click();
      const html = new DOMParser().parseFromString(await renderMarkdown(view.state.doc.toString()), "text/html");
      expect(html.querySelectorAll("thead th")).toHaveLength(3);
      expect(html.querySelector("tbody tr")?.textContent).toContain("keep");
    } finally { view.destroy(); parent.remove(); }
  });
});
