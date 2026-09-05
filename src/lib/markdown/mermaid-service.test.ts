import { afterEach, describe, expect, it, vi } from "vitest";
import type { MermaidConfig, RenderResult } from "mermaid";

const mocks = vi.hoisted(() => ({
  createClient: vi.fn(),
}));

vi.mock("./mermaid-renderer-client", () => ({
  createMermaidRendererClient: mocks.createClient,
}));

import { bundledMermaidIconPacks } from "./mermaid-icons";
import {
  MERMAID_RENDER_TIMEOUT_MS,
  disposeMermaidRenderer,
  renderMermaid,
} from "./mermaid-service";

function rendererClient() {
  return {
    render: vi.fn<(
      source: string,
      config: MermaidConfig,
      renderId: string,
      isCurrent?: () => boolean,
    ) => Promise<RenderResult>>(),
    destroy: vi.fn(),
  };
}

afterEach(() => {
  disposeMermaidRenderer();
  mocks.createClient.mockReset();
  vi.useRealTimers();
});

describe("Mermaid render service", () => {
  it("serializes configuration and reuses a healthy isolated renderer", async () => {
    const client = rendererClient();
    let completeFirst: (value: RenderResult) => void = () => undefined;
    client.render
      .mockReturnValueOnce(new Promise((resolve) => {
        completeFirst = resolve;
      }))
      .mockResolvedValueOnce({ svg: "<svg>second</svg>", diagramType: "flowchart-v2" });
    mocks.createClient.mockReturnValue(client);

    const firstConfig = {
      startOnLoad: false,
      securityLevel: "strict" as const,
      theme: "neutral" as const,
      fontFamily: "First Font",
    };
    const secondConfig = {
      startOnLoad: false,
      securityLevel: "strict" as const,
      theme: "dark" as const,
      fontFamily: "Second Font",
    };
    const first = renderMermaid("flowchart LR\nA", firstConfig, "first");
    await vi.waitFor(() => expect(client.render).toHaveBeenCalledOnce());
    const second = renderMermaid("flowchart LR\nB", secondConfig, "second");

    await Promise.resolve();
    expect(client.render).toHaveBeenCalledOnce();
    completeFirst({ svg: "<svg>first</svg>", diagramType: "flowchart-v2" });

    await expect(first).resolves.toEqual({
      svg: "<svg>first</svg>",
      diagramType: "flowchart-v2",
    });
    await expect(second).resolves.toEqual({
      svg: "<svg>second</svg>",
      diagramType: "flowchart-v2",
    });
    expect(mocks.createClient).toHaveBeenCalledOnce();
    expect(client.render.mock.calls.map(([, config]) => config))
      .toEqual([firstConfig, secondConfig]);
  });

  it("skips a superseded interactive render before it reaches the iframe", async () => {
    const client = rendererClient();
    let completeFirst: (value: RenderResult) => void = () => undefined;
    client.render
      .mockReturnValueOnce(new Promise((resolve) => {
        completeFirst = resolve;
      }))
      .mockResolvedValueOnce({ svg: "<svg>current</svg>", diagramType: "flowchart-v2" });
    mocks.createClient.mockReturnValue(client);
    let stale = false;

    const first = renderMermaid(
      "flowchart LR\nA",
      { startOnLoad: false, securityLevel: "strict" },
      "first",
    );
    await vi.waitFor(() => expect(client.render).toHaveBeenCalledOnce());
    const superseded = renderMermaid(
      "flowchart LR\nB",
      { startOnLoad: false, securityLevel: "strict" },
      "superseded",
      () => !stale,
    );
    const current = renderMermaid(
      "flowchart LR\nC",
      { startOnLoad: false, securityLevel: "strict" },
      "current",
    );
    stale = true;
    completeFirst({ svg: "<svg>first</svg>", diagramType: "flowchart-v2" });

    await expect(first).resolves.toEqual({
      svg: "<svg>first</svg>",
      diagramType: "flowchart-v2",
    });
    await expect(superseded).rejects.toMatchObject({ name: "AbortError" });
    await expect(current).resolves.toEqual({
      svg: "<svg>current</svg>",
      diagramType: "flowchart-v2",
    });
    expect(client.render).toHaveBeenCalledTimes(2);
  });

  it("destroys a permanently stalled realm and admits the next render", async () => {
    vi.useFakeTimers();
    const stalledClient = rendererClient();
    stalledClient.render.mockReturnValue(new Promise(() => undefined));
    const recoveredClient = rendererClient();
    recoveredClient.render.mockResolvedValue({
      svg: "<svg>recovered</svg>",
      diagramType: "flowchart-v2",
    });
    mocks.createClient
      .mockReturnValueOnce(stalledClient)
      .mockReturnValueOnce(recoveredClient);

    const stalled = renderMermaid(
      "flowchart LR\nA",
      { startOnLoad: false, securityLevel: "strict" },
      "stalled",
    );
    await vi.waitFor(() => expect(stalledClient.render).toHaveBeenCalledOnce());

    await vi.advanceTimersByTimeAsync(MERMAID_RENDER_TIMEOUT_MS);
    await expect(stalled).rejects.toThrow("Mermaid render timed out");
    expect(stalledClient.destroy).toHaveBeenCalledOnce();

    await expect(renderMermaid(
      "flowchart LR\nB",
      { startOnLoad: false, securityLevel: "strict" },
      "recovered",
    )).resolves.toEqual({
      svg: "<svg>recovered</svg>",
      diagramType: "flowchart-v2",
    });
    expect(recoveredClient.render).toHaveBeenCalledOnce();
  });

  it("keeps all documented icon packs in the isolated renderer bundle", () => {
    expect(bundledMermaidIconPacks).toEqual(expect.arrayContaining([
      expect.objectContaining({
        name: "inkflow",
        icons: expect.objectContaining({
          prefix: "inkflow",
          icons: { document: expect.any(Object) },
        }),
      }),
      expect.objectContaining({
        name: "logos",
        icons: expect.objectContaining({
          prefix: "logos",
          icons: { "github-icon": expect.any(Object) },
        }),
      }),
    ]));
  });
});
