import { afterEach, describe, expect, it, vi } from "vitest";
import { createMermaidRendererClient } from "./mermaid-renderer-client";
import { MERMAID_RENDERER_PROTOCOL } from "./mermaid-renderer-protocol";

afterEach(() => {
  document.querySelectorAll("iframe.inkflow-mermaid-renderer").forEach((frame) => frame.remove());
});

describe("isolated Mermaid renderer client", () => {
  it("waits for readiness and resolves only matching iframe responses", async () => {
    const client = createMermaidRendererClient();
    const frame = document.querySelector<HTMLIFrameElement>("iframe.inkflow-mermaid-renderer");
    expect(frame?.contentWindow).toBeTruthy();
    const postMessage = vi.spyOn(frame!.contentWindow!, "postMessage");
    const pending = client.render(
      "flowchart LR\nA",
      { startOnLoad: false, securityLevel: "strict" },
      "diagram-id",
    );
    expect(postMessage).not.toHaveBeenCalled();

    window.dispatchEvent(new MessageEvent("message", {
      source: frame!.contentWindow,
      data: { protocol: MERMAID_RENDERER_PROTOCOL, kind: "ready" },
    }));
    await vi.waitFor(() => expect(postMessage).toHaveBeenCalledOnce());
    const request = postMessage.mock.calls[0]?.[0] as { requestId: string };

    window.dispatchEvent(new MessageEvent("message", {
      source: window,
      data: {
        protocol: MERMAID_RENDERER_PROTOCOL,
        kind: "result",
        requestId: request.requestId,
        svg: "<svg>wrong source</svg>",
        diagramType: "flowchart-v2",
      },
    }));
    window.dispatchEvent(new MessageEvent("message", {
      source: frame!.contentWindow,
      data: {
        protocol: MERMAID_RENDERER_PROTOCOL,
        kind: "result",
        requestId: request.requestId,
        svg: "<svg>safe</svg>",
        diagramType: "flowchart-v2",
      },
    }));

    await expect(pending).resolves.toEqual({
      svg: "<svg>safe</svg>",
      diagramType: "flowchart-v2",
    });
    client.destroy();
    expect(frame!.isConnected).toBe(false);
  });

  it("rejects an in-flight render when its realm is destroyed", async () => {
    const client = createMermaidRendererClient();
    const frame = document.querySelector<HTMLIFrameElement>("iframe.inkflow-mermaid-renderer");
    window.dispatchEvent(new MessageEvent("message", {
      source: frame!.contentWindow,
      data: { protocol: MERMAID_RENDERER_PROTOCOL, kind: "ready" },
    }));
    const pending = client.render(
      "flowchart LR\nA",
      { startOnLoad: false, securityLevel: "strict" },
      "diagram-id",
    );
    await Promise.resolve();

    client.destroy(new Error("timed out"));

    await expect(pending).rejects.toThrow("timed out");
  });

  it("does not post a render that became stale while the iframe was loading", async () => {
    const client = createMermaidRendererClient();
    const frame = document.querySelector<HTMLIFrameElement>("iframe.inkflow-mermaid-renderer");
    const postMessage = vi.spyOn(frame!.contentWindow!, "postMessage");
    let current = true;
    const pending = client.render(
      "flowchart LR\nA",
      { startOnLoad: false, securityLevel: "strict" },
      "diagram-id",
      () => current,
    );

    current = false;
    window.dispatchEvent(new MessageEvent("message", {
      source: frame!.contentWindow,
      data: { protocol: MERMAID_RENDERER_PROTOCOL, kind: "ready" },
    }));

    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
    expect(postMessage).not.toHaveBeenCalled();
    client.destroy();
  });
});
