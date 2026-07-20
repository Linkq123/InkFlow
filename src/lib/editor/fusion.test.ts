import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { describe, expect, it } from "vitest";
import { fusionExtension, transformMarkdownTable } from "./fusion";

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

  it("loads titled images with only the Markdown destination", async () => {
    const source = "cursor\n\n![diagram](assets/diagram.png \"Architecture\")";
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

    expect(loaded).toContainEqual(["document-b", "assets/diagram.png"]);

    view.destroy();
    parent.remove();
  });
});
