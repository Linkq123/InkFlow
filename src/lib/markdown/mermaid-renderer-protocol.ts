import type { MermaidConfig } from "mermaid";

export const MERMAID_RENDERER_PROTOCOL = "inkflow.mermaid-renderer/v1" as const;

export interface MermaidRendererReady {
  protocol: typeof MERMAID_RENDERER_PROTOCOL;
  kind: "ready";
}

export interface MermaidRendererFatal {
  protocol: typeof MERMAID_RENDERER_PROTOCOL;
  kind: "fatal";
  error: string;
}

export interface MermaidRendererRequest {
  protocol: typeof MERMAID_RENDERER_PROTOCOL;
  kind: "render";
  requestId: string;
  source: string;
  config: MermaidConfig;
  renderId: string;
}

export interface MermaidRendererResponse {
  protocol: typeof MERMAID_RENDERER_PROTOCOL;
  kind: "result";
  requestId: string;
  svg?: string;
  diagramType?: string;
  error?: string;
}

export type MermaidRendererMessage =
  | MermaidRendererReady
  | MermaidRendererFatal
  | MermaidRendererResponse;
