import mermaid from "mermaid";
import { bundledMermaidIconPacks } from "./lib/markdown/mermaid-icons";
import {
  MERMAID_RENDERER_PROTOCOL,
  type MermaidRendererRequest,
} from "./lib/markdown/mermaid-renderer-protocol";

function post(message: object): void {
  const origin = window.location.origin;
  window.parent.postMessage(
    { protocol: MERMAID_RENDERER_PROTOCOL, ...message },
    origin === "null" ? "*" : origin,
  );
}

window.addEventListener("message", (event: MessageEvent<MermaidRendererRequest>) => {
  if (event.source !== window.parent) return;
  const request = event.data;
  if (
    !request
    || request.protocol !== MERMAID_RENDERER_PROTOCOL
    || request.kind !== "render"
  ) {
    return;
  }
  void (async () => {
    try {
      mermaid.initialize(request.config);
      const result = await mermaid.render(request.renderId, request.source);
      post({
        kind: "result",
        requestId: request.requestId,
        svg: result.svg,
        diagramType: result.diagramType,
      });
    } catch (error) {
      post({
        kind: "result",
        requestId: request.requestId,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  })();
});

try {
  mermaid.registerIconPacks(bundledMermaidIconPacks);
  post({ kind: "ready" });
} catch (error) {
  post({
    kind: "fatal",
    error: error instanceof Error ? error.message : String(error),
  });
}
