import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./pipeline";
import { serializeMarkdownImage } from "./serialize-image";

describe("Markdown image serialization", () => {
  it.each(["photo]", "photo[", "photo\\", "[photo]", "*photo*", "&copy;", "`photo`"])("renders the literal label %s", async label => {
    const html = await renderMarkdown(serializeMarkdownImage(label, "note.assets/image.png"));
    const container = document.createElement("div");
    container.innerHTML = html;
    expect(container.querySelectorAll("img")).toHaveLength(1);
    expect(container.querySelector("img")?.getAttribute("alt")).toBe(label);
    expect(container.querySelector("img")?.getAttribute("src")).toBe("note.assets/image.png");
  });

  it.each([
    ["Copy name.assets/image.png", "Copy%20name.assets/image.png"],
    ["Copy).assets/image.png", "Copy).assets/image.png"],
    ["Copy%26name.assets/image.png", "Copy%26name.assets/image.png"],
    ["Copy<name.assets/image.png", "Copy%3Cname.assets/image.png"],
  ])("preserves the destination %s", async (path, expected) => {
    const container = document.createElement("div");
    container.innerHTML = await renderMarkdown(serializeMarkdownImage("image", path));
    expect(container.querySelectorAll("img")).toHaveLength(1);
    expect(container.querySelector("img")?.getAttribute("src")).toBe(expected);
  });
});
