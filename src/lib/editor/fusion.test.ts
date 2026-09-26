import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { markdown } from "@codemirror/lang-markdown";
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

describe("Markdown table commands", () => {
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
      expect(view.state.doc.toString()).toBe("cursor\n\n| Name | Ready |\n| --- | :---: |\n# Keep | heading");
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
