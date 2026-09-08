import { mount, unmount } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import MarkdownPreview from "../components/MarkdownPreview.svelte";
import { prepareExportDocument } from "./export-document";
import { renderMarkdown } from "./pipeline";

const { loadResource } = vi.hoisted(() => ({ loadResource: vi.fn() }));
vi.mock("../api/client", () => ({ api: { loadResource }, isDesktop: () => true }));

// Keep the real renderer and sanitizer. Only the filesystem/IPC boundary is
// replaced; JSDOM uses render-service's normal Worker-unavailable fallback.
const dataUrl = "data:image/png;base64,aW1hZ2U=";
const markdown = [
  "![draft](inkflow-asset://draft.png)",
  '<picture><source srcset="inkflow-asset://wide.png 2x">'
    + '<img src="inkflow-asset://fallback.png" srcset="inkflow-asset://small.png 1x"></picture>',
].join("\n\n");
const sources = ["draft.png", "wide.png", "fallback.png", "small.png"]
  .map(name => `inkflow-asset://${name}`);

afterEach(() => {
  loadResource.mockReset();
  document.body.replaceChildren();
});

describe("pending images through the real Markdown pipeline", () => {
  it.each(["new-draft-id", "restored-copy-id"])("hydrates preview images in document scope %s", async (documentId) => {
    loadResource.mockResolvedValue(dataUrl);
    const target = document.createElement("div");
    document.body.append(target);
    const component = mount(MarkdownPreview, { target, props: { value: markdown, documentId } });
    try {
      await vi.waitFor(() => {
        for (const source of sources) expect(loadResource).toHaveBeenCalledWith(documentId, source);
        expect(target.querySelector('img[alt="draft"]')?.getAttribute("src")).toBe(dataUrl);
        expect(target.querySelector("picture img")?.getAttribute("src")).toBe(dataUrl);
        expect(target.querySelector("picture img")?.getAttribute("srcset")).toBe(`${dataUrl} 1x`);
        expect(target.querySelector("source")?.getAttribute("srcset")).toBe(`${dataUrl} 2x`);
      });
    } finally { await unmount(component); }
  });

  it("embeds unsaved and restored image references in exported HTML", async () => {
    const loadExportResource = vi.fn(async () => dataUrl);
    const html = await prepareExportDocument(markdown, {
      allowRemoteImages: false, editorFont: "sans-serif", loadResource: loadExportResource,
    });
    for (const source of sources) expect(loadExportResource).toHaveBeenCalledWith(source);
    expect(loadExportResource).toHaveBeenCalledTimes(sources.length);
    const result = new DOMParser().parseFromString(html, "text/html");
    expect(result.querySelector('img[alt="draft"]')?.getAttribute("src")).toBe(dataUrl);
    expect(result.querySelector("picture img")?.getAttribute("src")).toBe(dataUrl);
    expect(result.querySelector("picture img")?.getAttribute("srcset")).toBe(`${dataUrl} 1x`);
    expect(result.querySelector("source")?.getAttribute("srcset")).toBe(`${dataUrl} 2x`);
    expect(html).not.toContain("inkflow-asset:");
  });

  it.each(["javascript:alert(1)", "file:///private.png", "data:image/png;base64,AA==", "custom:image.png"])(
    "continues rejecting the image protocol in %s", async (source) => {
      const html = await renderMarkdown(`<picture><source srcset="${source} 2x"><img src="${source}" srcset="${source} 1x"></picture>`);
      const result = new DOMParser().parseFromString(html, "text/html");
      expect(result.querySelector("img")?.hasAttribute("src")).toBe(false);
      expect(result.querySelector("img")?.hasAttribute("srcset")).toBe(false);
      expect(result.querySelector("source")?.hasAttribute("srcset")).toBe(false);
    },
  );
});
