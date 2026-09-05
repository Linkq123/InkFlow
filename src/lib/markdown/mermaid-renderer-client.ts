import type { MermaidConfig, RenderResult } from "mermaid";
import {
  MERMAID_RENDERER_PROTOCOL,
  type MermaidRendererMessage,
  type MermaidRendererRequest,
} from "./mermaid-renderer-protocol";

export interface MermaidRendererClient {
  render(
    source: string,
    config: MermaidConfig,
    renderId: string,
    isCurrent?: () => boolean,
  ): Promise<RenderResult>;
  destroy(reason?: Error): void;
}

interface PendingRender {
  resolve: (result: RenderResult) => void;
  reject: (error: Error) => void;
}

class IframeMermaidRendererClient implements MermaidRendererClient {
  private readonly frame: HTMLIFrameElement;
  private readonly ready: Promise<void>;
  private readonly pending = new Map<string, PendingRender>();
  private resolveReady: () => void = () => undefined;
  private rejectReady: (error: Error) => void = () => undefined;
  private destroyed = false;

  constructor() {
    if (typeof document === "undefined" || typeof window === "undefined") {
      throw new Error("The isolated Mermaid renderer requires a browser document.");
    }
    this.ready = new Promise<void>((resolve, reject) => {
      this.resolveReady = resolve;
      this.rejectReady = reject;
    });
    this.frame = document.createElement("iframe");
    this.frame.className = "inkflow-mermaid-renderer";
    this.frame.title = "";
    this.frame.tabIndex = -1;
    this.frame.setAttribute("aria-hidden", "true");
    this.frame.style.position = "fixed";
    this.frame.style.left = "-10000px";
    this.frame.style.top = "-10000px";
    this.frame.style.width = "1024px";
    this.frame.style.height = "768px";
    this.frame.style.border = "0";
    this.frame.style.opacity = "0";
    this.frame.style.pointerEvents = "none";
    this.frame.src = new URL("mermaid-renderer.html", document.baseURI).href;
    window.addEventListener("message", this.handleMessage);
    this.frame.addEventListener("error", this.handleFrameError);
    document.body.append(this.frame);
  }

  async render(
    source: string,
    config: MermaidConfig,
    renderId: string,
    isCurrent: () => boolean = () => true,
  ): Promise<RenderResult> {
    await this.ready;
    if (this.destroyed) throw new Error("The isolated Mermaid renderer was disposed.");
    if (!isCurrent()) throw new DOMException("Superseded Mermaid render", "AbortError");
    const requestId = crypto.randomUUID();
    return await new Promise<RenderResult>((resolve, reject) => {
      this.pending.set(requestId, { resolve, reject });
      const request: MermaidRendererRequest = {
        protocol: MERMAID_RENDERER_PROTOCOL,
        kind: "render",
        requestId,
        source,
        config,
        renderId,
      };
      try {
        const target = this.frame.contentWindow;
        if (!target) throw new Error("The isolated Mermaid renderer is unavailable.");
        target.postMessage(request, messageTargetOrigin(this.frame.src));
      } catch (error) {
        this.pending.delete(requestId);
        reject(asError(error));
      }
    });
  }

  destroy(reason = new Error("The isolated Mermaid renderer was disposed.")): void {
    if (this.destroyed) return;
    this.destroyed = true;
    window.removeEventListener("message", this.handleMessage);
    this.frame.removeEventListener("error", this.handleFrameError);
    this.frame.remove();
    this.rejectReady(reason);
    for (const request of this.pending.values()) request.reject(reason);
    this.pending.clear();
  }

  private readonly handleFrameError = (): void => {
    this.destroy(new Error("The isolated Mermaid renderer failed to load."));
  };

  private readonly handleMessage = (event: MessageEvent<MermaidRendererMessage>): void => {
    if (event.source !== this.frame.contentWindow) return;
    const message = event.data;
    if (!message || message.protocol !== MERMAID_RENDERER_PROTOCOL) return;
    if (message.kind === "ready") {
      this.resolveReady();
      return;
    }
    if (message.kind === "fatal") {
      this.destroy(new Error(message.error));
      return;
    }
    const request = this.pending.get(message.requestId);
    if (!request) return;
    this.pending.delete(message.requestId);
    if (message.error) {
      request.reject(new Error(message.error));
      return;
    }
    request.resolve({
      svg: message.svg ?? "",
      diagramType: message.diagramType ?? "unknown",
    });
  };
}

export function createMermaidRendererClient(): MermaidRendererClient {
  return new IframeMermaidRendererClient();
}

function messageTargetOrigin(source: string): string {
  const origin = new URL(source).origin;
  return origin === "null" ? "*" : origin;
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}
