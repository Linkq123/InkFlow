import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { markdown } from "@codemirror/lang-markdown";
import { afterEach, describe, expect, it, vi } from "vitest";
import { collectFencedBlocks, collectViewportBlocks, fusionExtension, transformMarkdownTable } from "./fusion";

const mocks = vi.hoisted(() => ({
  detectRemoteMermaidImage: vi.fn(async () => false),
  mermaidInitialize: vi.fn(),
  mermaidRender: vi.fn(async () => ({ svg: "<svg></svg>" })),
}));

vi.mock("../markdown/resources", async (importOriginal) => ({
  ...await importOriginal<typeof import("../markdown/resources")>(),
  hasRemoteMermaidImageReference: mocks.detectRemoteMermaidImage,
}));

vi.mock("mermaid", () => ({
  default: {
    initialize: mocks.mermaidInitialize,
    render: mocks.mermaidRender,
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
