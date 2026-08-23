import { createClassComponent } from "svelte/legacy";
import { afterEach, describe, expect, it, vi } from "vitest";
import MarkdownPreview from "./MarkdownPreview.svelte";

const mocks = vi.hoisted(() => ({
  loadResource: vi.fn(),
  mermaidInitialize: vi.fn(),
  mermaidRender: vi.fn(),
  renderInWorker: vi.fn(),
}));

vi.mock("../api/client", () => ({
  api: { loadResource: mocks.loadResource },
  isDesktop: () => true,
}));

vi.mock("../markdown/render-service", () => ({
  renderInWorker: mocks.renderInWorker,
}));

vi.mock("mermaid", () => ({
  default: {
    initialize: mocks.mermaidInitialize,
    render: mocks.mermaidRender,
  },
}));

afterEach(() => {
  document.body.replaceChildren();
  vi.clearAllMocks();
});

describe("MarkdownPreview", () => {
  it("hydrates local responsive image candidates through the scoped loader", async () => {
    const resources = new Map([
      ["wide.png", "data:image/png;base64,d2lkZQ=="],
      ["small.png", "data:image/png;base64,c21hbGw="],
      ["large.png", "data:image/png;base64,bGFyZ2U="],
      ["fallback.png", "data:image/png;base64,ZmFsbGJhY2s="],
    ]);
    mocks.loadResource.mockImplementation(async (_documentId: string, source: string) => {
      const loaded = resources.get(source);
      if (!loaded) throw new Error("missing fixture");
      return loaded;
    });
    mocks.renderInWorker.mockResolvedValue(
      '<picture><source srcset="wide.png 2x"><img src="fallback.png" srcset="small.png 1x, large.png 2x" alt="responsive"></picture>',
    );

    const target = document.createElement("div");
    document.body.append(target);
    const component = createClassComponent({
      component: MarkdownPreview,
      target,
      props: {
        value: "responsive",
        documentId: "document-1",
        allowRemoteImages: false,
      },
    });

    await vi.waitFor(() => {
      expect(target.querySelector("source")?.getAttribute("srcset"))
        .toBe("data:image/png;base64,d2lkZQ== 2x");
      expect(target.querySelector("img")?.getAttribute("srcset"))
        .toBe("data:image/png;base64,c21hbGw= 1x, data:image/png;base64,bGFyZ2U= 2x");
      expect(target.querySelector("img")?.getAttribute("src"))
        .toBe("data:image/png;base64,ZmFsbGJhY2s=");
    });
    expect(mocks.loadResource).toHaveBeenCalledTimes(4);
    component.$destroy();
  });

  it("uses a local srcset when its remote fallback is blocked", async () => {
    mocks.loadResource.mockResolvedValue("data:image/png;base64,bG9jYWw=");
    mocks.renderInWorker.mockResolvedValue(
      '<img src="https://example.com/fallback.png" srcset="local.png 1x, https://example.com/remote.png 2x" alt="responsive">',
    );

    const target = document.createElement("div");
    document.body.append(target);
    const component = createClassComponent({
      component: MarkdownPreview,
      target,
      props: {
        value: "responsive",
        documentId: "document-1",
        allowRemoteImages: false,
      },
    });

    await vi.waitFor(() => {
      const image = target.querySelector("img");
      expect(image?.getAttribute("srcset"))
        .toBe("data:image/png;base64,bG9jYWw= 1x");
      expect(image?.hasAttribute("src")).toBe(false);
      expect(image?.classList.contains("remote-blocked")).toBe(false);
    });
    expect(mocks.loadResource).toHaveBeenCalledWith("document-1", "local.png");
    component.$destroy();
  });

  it("uses a local fallback without blocked styling when srcset is remote-only", async () => {
    mocks.loadResource.mockResolvedValue("data:image/png;base64,bG9jYWw=");
    mocks.renderInWorker.mockResolvedValue(
      '<img src="local.png" srcset="https://example.com/remote.png 2x" alt="responsive">',
    );

    const target = document.createElement("div");
    document.body.append(target);
    const component = createClassComponent({
      component: MarkdownPreview,
      target,
      props: {
        value: "responsive",
        documentId: "document-1",
        allowRemoteImages: false,
      },
    });

    await vi.waitFor(() => {
      const image = target.querySelector("img");
      expect(image?.getAttribute("src")).toBe("data:image/png;base64,bG9jYWw=");
      expect(image?.hasAttribute("srcset")).toBe(false);
      expect(image?.classList.contains("remote-blocked")).toBe(false);
    });
    expect(mocks.loadResource).toHaveBeenCalledWith("document-1", "local.png");
    component.$destroy();
  });

  it("shows the blocked fallback when an image only has remote srcset candidates", async () => {
    mocks.renderInWorker.mockResolvedValue(
      '<img srcset="https://example.com/remote.png 2x" alt="responsive">',
    );

    const target = document.createElement("div");
    document.body.append(target);
    const component = createClassComponent({
      component: MarkdownPreview,
      target,
      props: {
        value: "responsive",
        documentId: "document-1",
        allowRemoteImages: false,
      },
    });

    await vi.waitFor(() => {
      const image = target.querySelector("img");
      expect(image?.hasAttribute("srcset")).toBe(false);
      expect(image?.classList.contains("remote-blocked")).toBe(true);
      expect(image?.alt).toBe("Remote image blocked: responsive");
    });
    expect(mocks.loadResource).not.toHaveBeenCalled();
    component.$destroy();
  });

  it("uses a picture source when its remote fallback is blocked", async () => {
    mocks.loadResource.mockResolvedValue("data:image/png;base64,bG9jYWw=");
    mocks.renderInWorker.mockResolvedValue(
      '<picture><source srcset="local.png 1x"><img src="https://example.com/fallback.png" alt="responsive"></picture>',
    );

    const target = document.createElement("div");
    document.body.append(target);
    const component = createClassComponent({
      component: MarkdownPreview,
      target,
      props: {
        value: "responsive",
        documentId: "document-1",
        allowRemoteImages: false,
      },
    });

    await vi.waitFor(() => {
      expect(target.querySelector("source")?.getAttribute("srcset"))
        .toBe("data:image/png;base64,bG9jYWw= 1x");
      const image = target.querySelector("picture > img");
      expect(image).not.toBeNull();
      expect(image?.hasAttribute("src")).toBe(false);
      expect(image?.classList.contains("remote-blocked")).toBe(false);
    });
    component.$destroy();
  });

  it("updates a blocked fallback when the picture source media starts or stops matching", async () => {
    const originalMatchMedia = window.matchMedia;
    let matches = false;
    const changeListeners = new Set<() => void>();
    const addEventListener = vi.fn((_event: string, listener: () => void) => {
      changeListeners.add(listener);
    });
    const removeEventListener = vi.fn((_event: string, listener: () => void) => {
      changeListeners.delete(listener);
    });
    window.matchMedia = vi.fn((query: string) => ({
      get matches() {
        return matches;
      },
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener,
      removeEventListener,
      dispatchEvent: vi.fn(),
    }));
    mocks.loadResource.mockResolvedValue("data:image/png;base64,bG9jYWw=");
    mocks.renderInWorker.mockResolvedValue(
      '<picture><source media="(min-width: 9999px)" srcset="local.png"><img src="https://example.com/fallback.png" alt="responsive"></picture>',
    );

    const target = document.createElement("div");
    document.body.append(target);
    const component = createClassComponent({
      component: MarkdownPreview,
      target,
      props: {
        value: "responsive",
        documentId: "document-1",
        allowRemoteImages: false,
      },
    });

    try {
      await vi.waitFor(() => {
        const image = target.querySelector<HTMLImageElement>("picture > img");
        expect(image?.classList.contains("remote-blocked")).toBe(true);
        expect(image?.alt).toBe("Remote image blocked: responsive");
      });
      expect(addEventListener).toHaveBeenCalledWith("change", expect.any(Function));

      matches = true;
      for (const listener of changeListeners) listener();
      await vi.waitFor(() => {
        const image = target.querySelector<HTMLImageElement>("picture > img");
        expect(image?.classList.contains("remote-blocked")).toBe(false);
        expect(image?.alt).toBe("responsive");
      });

      matches = false;
      for (const listener of changeListeners) listener();
      await vi.waitFor(() => {
        const image = target.querySelector<HTMLImageElement>("picture > img");
        expect(image?.classList.contains("remote-blocked")).toBe(true);
        expect(image?.alt).toBe("Remote image blocked: responsive");
      });
    } finally {
      component.$destroy();
      expect(removeEventListener).toHaveBeenCalledWith("change", expect.any(Function));
      window.matchMedia = originalMatchMedia;
    }
  });

  it("uses a picture source when its local fallback is missing", async () => {
    mocks.loadResource.mockImplementation(async (_documentId: string, source: string) => {
      if (source === "fallback.png") throw new Error("missing fixture");
      return "data:image/png;base64,bG9jYWw=";
    });
    mocks.renderInWorker.mockResolvedValue(
      '<picture><source srcset="local.png 1x"><img src="fallback.png" alt="responsive"></picture>',
    );

    const target = document.createElement("div");
    document.body.append(target);
    const component = createClassComponent({
      component: MarkdownPreview,
      target,
      props: {
        value: "responsive",
        documentId: "document-1",
        allowRemoteImages: false,
      },
    });

    await vi.waitFor(() => {
      expect(target.querySelector("source")?.getAttribute("srcset"))
        .toBe("data:image/png;base64,bG9jYWw= 1x");
      const image = target.querySelector("picture > img");
      expect(image).not.toBeNull();
      expect(image?.hasAttribute("src")).toBe(false);
      expect(image?.classList.contains("resource-missing")).toBe(false);
    });
    component.$destroy();
  });

  it("does not hydrate replacement DOM with an older remote-image permission", async () => {
    let finishLocalImage: (value: string) => void = () => undefined;
    mocks.loadResource.mockReturnValue(new Promise<string>((resolve) => {
      finishLocalImage = resolve;
    }));
    mocks.renderInWorker.mockImplementation(async (markdown: string) => markdown === "trusted"
      ? [
          '<img src="local.png" alt="local">',
          '<pre><code class="language-mermaid">flowchart LR\nA--&gt;B</code></pre>',
        ].join("")
      : [
          '<pre><code class="language-mermaid">sequenceDiagram\n',
          'participant A@{ icon: &quot;https://example.com/a.png&quot; }</code></pre>',
        ].join(""));

    const target = document.createElement("div");
    document.body.append(target);
    const component = createClassComponent({
      component: MarkdownPreview,
      target,
      props: {
        value: "trusted",
        documentId: "document-1",
        allowRemoteImages: true,
      },
    });

    await vi.waitFor(() => expect(mocks.loadResource).toHaveBeenCalledOnce());
    component.$set({ value: "untrusted", allowRemoteImages: false });
    await vi.waitFor(() => {
      expect(target.querySelector("pre")?.getAttribute("data-error"))
        .toBe("Remote Mermaid image blocked");
    });

    finishLocalImage("data:image/png;base64,AA==");
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(mocks.mermaidRender).not.toHaveBeenCalled();
    component.$destroy();
  });
});
