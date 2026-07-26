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
