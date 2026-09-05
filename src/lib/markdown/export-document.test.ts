import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { prepareExportDocument } from "./export-document";

const { mermaidRender } = vi.hoisted(() => ({
  mermaidRender: vi.fn(),
}));

vi.mock("./render-service", () => ({
  renderForExportInWorker: async (markdown: string) => markdown,
}));

vi.mock("./mermaid-service", () => ({
  renderMermaid: (source: string, _config: unknown, idPrefix: string) =>
    mermaidRender(`${idPrefix}-test`, source),
}));

describe("prepareExportDocument", () => {
  beforeEach(() => mermaidRender.mockReset());
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("blocks remote images and resolves local images through the scoped loader", async () => {
    const loadResource = vi.fn(async () => "data:image/png;base64,aW1hZ2U=");
    const html = await prepareExportDocument(
      '<p><img src="local.png" alt="local"><img src="https://example.com/x.png" alt="remote"></p>',
      { allowRemoteImages: false, editorFont: "sans-serif", loadResource },
    );

    expect(loadResource).toHaveBeenCalledWith("local.png");
    expect(html).toContain("data:image/png;base64,aW1hZ2U=");
    expect(html).toContain("Remote image blocked: remote");
    expect(html).not.toContain('src="https://example.com');
  });

  it("embeds every local picture srcset candidate and preserves descriptors", async () => {
    const resources = new Map([
      ["large.png", "data:image/png;base64,bGFyZ2U="],
      ["small.png", "data:image/png;base64,c21hbGw="],
      ["fallback.png", "data:image/png;base64,ZmFsbGJhY2s="],
    ]);
    const loadResource = vi.fn(async (source: string) => {
      const data = resources.get(source);
      if (!data) throw new Error("missing fixture");
      return data;
    });
    const html = await prepareExportDocument(
      '<picture><source srcset="large.png 2x, small.png 1x"><img src="fallback.png" alt="responsive"></picture>',
      { allowRemoteImages: false, editorFont: "sans-serif", loadResource },
    );

    expect(loadResource).toHaveBeenCalledTimes(3);
    expect(html).toContain(
      'srcset="data:image/png;base64,bGFyZ2U= 2x, data:image/png;base64,c21hbGw= 1x"',
    );
    expect(html).toContain('src="data:image/png;base64,ZmFsbGJhY2s="');
    expect(html).not.toContain("large.png");
    expect(html).not.toContain("small.png");
    expect(html).not.toContain("fallback.png");
  });

  it("drops a missing srcset candidate so the embedded fallback remains usable", async () => {
    const loadResource = vi.fn(async (source: string) => {
      if (source === "missing.png") throw new Error("missing fixture");
      return "data:image/png;base64,ZmFsbGJhY2s=";
    });
    const html = await prepareExportDocument(
      '<picture><source srcset="missing.png 2x"><img src="fallback.png" alt="responsive"></picture>',
      { allowRemoteImages: false, editorFont: "sans-serif", loadResource },
    );

    expect(html).not.toContain("srcset");
    expect(html).toContain('src="data:image/png;base64,ZmFsbGJhY2s="');
  });

  it("embeds local srcset candidates while keeping remote candidates inert", async () => {
    const loadResource = vi.fn(async () => "data:image/png;base64,bG9jYWw=");
    const html = await prepareExportDocument(
      '<picture><source srcset="local.png 1x, https://example.com/remote.png 2x"><img src="fallback.png" alt="responsive"></picture>',
      { allowRemoteImages: false, editorFont: "sans-serif", loadResource },
    );
    const documentNode = new DOMParser().parseFromString(html, "text/html");
    const source = documentNode.querySelector("source");

    expect(loadResource).toHaveBeenCalledWith("local.png");
    expect(source?.getAttribute("srcset"))
      .toBe("data:image/png;base64,bG9jYWw= 1x");
    expect(source?.getAttribute("data-inkflow-remote-srcset"))
      .toBe("https://example.com/remote.png 2x");
    expect(source?.getAttribute("srcset")).not.toContain("https://");
  });

  it("keeps a local img srcset when its remote fallback is blocked", async () => {
    const loadResource = vi.fn(async () => "data:image/png;base64,bG9jYWw=");
    const html = await prepareExportDocument(
      '<img src="https://example.com/fallback.png" srcset="local.png 1x, https://example.com/remote.png 2x" alt="responsive">',
      { allowRemoteImages: false, editorFont: "sans-serif", loadResource },
    );
    const image = new DOMParser()
      .parseFromString(html, "text/html")
      .querySelector("img");

    expect(image?.hasAttribute("src")).toBe(false);
    expect(image?.getAttribute("srcset"))
      .toBe("data:image/png;base64,bG9jYWw= 1x");
    expect(image?.getAttribute("data-inkflow-remote-src"))
      .toBe("https://example.com/fallback.png");
    expect(image?.getAttribute("data-inkflow-remote-srcset"))
      .toBe("https://example.com/remote.png 2x");
  });

  it("embeds a local fallback without blocked styling when srcset is remote-only", async () => {
    const html = await prepareExportDocument(
      '<img src="local.png" srcset="https://example.com/remote.png 2x" alt="responsive">',
      {
        allowRemoteImages: false,
        editorFont: "sans-serif",
        loadResource: async () => "data:image/png;base64,bG9jYWw=",
      },
    );
    const image = new DOMParser()
      .parseFromString(html, "text/html")
      .querySelector("img");

    expect(image?.getAttribute("src")).toBe("data:image/png;base64,bG9jYWw=");
    expect(image?.hasAttribute("srcset")).toBe(false);
    expect(image?.classList.contains("remote-blocked")).toBe(false);
  });

  it("replaces an image whose only sources are remote srcset candidates", async () => {
    const html = await prepareExportDocument(
      '<img srcset="https://example.com/remote.png 2x" alt="responsive">',
      { allowRemoteImages: false, editorFont: "sans-serif" },
    );

    expect(html).toContain("[Remote image blocked: responsive]");
    expect(html).not.toContain("<img");
    expect(html).not.toContain("https://example.com/remote.png");
  });

  it("keeps a picture source when its remote img fallback is blocked", async () => {
    const loadResource = vi.fn(async () => "data:image/png;base64,bG9jYWw=");
    const html = await prepareExportDocument(
      '<picture><source srcset="local.png 1x"><img src="https://example.com/fallback.png" alt="responsive"></picture>',
      { allowRemoteImages: false, editorFont: "sans-serif", loadResource },
    );
    const documentNode = new DOMParser().parseFromString(html, "text/html");
    const image = documentNode.querySelector<HTMLImageElement>("picture > img");

    expect(documentNode.querySelector("source")?.getAttribute("srcset"))
      .toBe("data:image/png;base64,bG9jYWw= 1x");
    expect(image).not.toBeNull();
    expect(image?.hasAttribute("src")).toBe(false);
    expect(image?.classList.contains("remote-blocked")).toBe(false);
    expect(image?.alt).toBe("responsive");
    expect(html).not.toContain("Remote image blocked:");
  });

  it("preserves media-conditional picture sources for the exported viewport", async () => {
    const html = await prepareExportDocument(
      '<picture><source media="(min-width: 9999px)" srcset="local.png"><img src="https://example.com/fallback.png" alt="responsive"></picture>',
      {
        allowRemoteImages: false,
        editorFont: "sans-serif",
        loadResource: async () => "data:image/png;base64,bG9jYWw=",
      },
    );
    const documentNode = new DOMParser().parseFromString(html, "text/html");
    const source = documentNode.querySelector("picture > source");
    const image = documentNode.querySelector<HTMLImageElement>("picture > img");

    expect(source?.getAttribute("media")).toBe("(min-width: 9999px)");
    expect(source?.getAttribute("srcset"))
      .toBe("data:image/png;base64,bG9jYWw=");
    expect(image).not.toBeNull();
    expect(image?.hasAttribute("src")).toBe(false);
    expect(image?.alt).toBe("responsive");
    expect(html).not.toContain("Remote image blocked:");
  });

  it("keeps a picture source when its local img fallback is missing", async () => {
    const loadResource = vi.fn(async (source: string) => {
      if (source === "fallback.png") throw new Error("missing fixture");
      return "data:image/png;base64,bG9jYWw=";
    });
    const html = await prepareExportDocument(
      '<picture><source srcset="local.png 1x"><img src="fallback.png" alt="responsive"></picture>',
      { allowRemoteImages: false, editorFont: "sans-serif", loadResource },
    );
    const documentNode = new DOMParser().parseFromString(html, "text/html");
    const image = documentNode.querySelector<HTMLImageElement>("picture > img");

    expect(documentNode.querySelector("source")?.getAttribute("srcset"))
      .toBe("data:image/png;base64,bG9jYWw= 1x");
    expect(image).not.toBeNull();
    expect(image?.hasAttribute("src")).toBe(false);
    expect(image?.alt).toBe("responsive");
    expect(html).not.toContain("Missing image:");
  });

  it("embeds local image references created by Mermaid", async () => {
    mermaidRender.mockResolvedValue({
      svg: '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image href="diagram.png"></image><image xlink:href="legacy.png"></image></svg>',
    });
    const resources = new Map([
      ["diagram.png", "data:image/png;base64,ZGlhZ3JhbQ=="],
      ["legacy.png", "data:image/png;base64,bGVnYWN5"],
    ]);
    const loadResource = vi.fn(async (source: string) => {
      const data = resources.get(source);
      if (!data) throw new Error("missing fixture");
      return data;
    });

    const html = await prepareExportDocument(
      '<pre><code class="language-mermaid">graph TD</code></pre>',
      { allowRemoteImages: false, editorFont: "sans-serif", loadResource },
    );

    expect(loadResource).toHaveBeenCalledWith("diagram.png");
    expect(loadResource).toHaveBeenCalledWith("legacy.png");
    expect(html).toContain("data:image/png;base64,ZGlhZ3JhbQ==");
    expect(html).toContain("data:image/png;base64,bGVnYWN5");
    expect(html).not.toContain("diagram.png");
    expect(html).not.toContain("legacy.png");
  });

  it("resolves local Mermaid metadata images before rendering", async () => {
    const embedded = "data:image/png;base64,bG9nbw==";
    const loadResource = vi.fn(async () => embedded);
    const placeholderSvg = [
      '<svg xmlns="http://www.w3.org/2000/svg"',
      ' width="1024" height="576" viewBox="0 0 1024 576">',
      "<desc>inkflow-resource-0</desc></svg>",
    ].join("");
    const placeholder = `data:image/svg+xml;base64,${btoa(placeholderSvg)}`;
    const decode = vi.fn(async () => undefined);
    vi.stubGlobal("Image", class {
      src = "";
      naturalWidth = 1024;
      naturalHeight = 576;
      decode = decode;
    });
    const createObjectURL = vi.spyOn(URL, "createObjectURL")
      .mockReturnValue("blob:inkflow-mermaid-image");
    const revokeObjectURL = vi.spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => undefined);
    mermaidRender.mockResolvedValue({
      svg: `<svg xmlns="http://www.w3.org/2000/svg"><image href="${placeholder}"></image></svg>`,
    });

    const html = await prepareExportDocument(
      '<pre><code class="language-mermaid">flowchart LR\n  A@{ img: "logo.png", label: "A", pos: "b" }</code></pre>',
      { allowRemoteImages: false, editorFont: "sans-serif", loadResource },
    );

    expect(loadResource).toHaveBeenCalledOnce();
    expect(loadResource).toHaveBeenCalledWith("logo.png");
    expect(mermaidRender).toHaveBeenCalledOnce();
    expect(mermaidRender.mock.calls[0]?.[1]).toContain(placeholder);
    expect(mermaidRender.mock.calls[0]?.[1]).not.toContain("logo.png");
    expect(createObjectURL).toHaveBeenCalledOnce();
    expect(decode).toHaveBeenCalledOnce();
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:inkflow-mermaid-image");
    expect(html).toContain('class="mermaid-diagram"');
    expect(html).toContain(embedded);
    expect(html).not.toContain(placeholder);
  });
});
