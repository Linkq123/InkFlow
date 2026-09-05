import type { MermaidConfig, RenderResult } from "mermaid";
import {
  createMermaidRendererClient,
  type MermaidRendererClient,
} from "./mermaid-renderer-client";

export const MERMAID_RENDER_TIMEOUT_MS = 30_000;

let renderTail: Promise<void> = Promise.resolve();
let renderer: MermaidRendererClient | null = null;

function currentRenderer(): MermaidRendererClient {
  renderer ??= createMermaidRendererClient();
  return renderer;
}

function discardRenderer(candidate: MermaidRendererClient, reason?: Error): void {
  if (renderer === candidate) renderer = null;
  candidate.destroy(reason);
}

function renderWithTimeout(
  client: MermaidRendererClient,
  operation: Promise<RenderResult>,
): Promise<RenderResult> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      const error = new Error("Mermaid render timed out.");
      // The Mermaid singleton lives inside this iframe. Removing the frame
      // tears down the entire realm, so a permanently hung Promise cannot
      // overlap or poison the replacement used by the next queued render.
      discardRenderer(client, error);
      reject(error);
    }, MERMAID_RENDER_TIMEOUT_MS);

    void operation.then(
      (result) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve(result);
      },
      (error: unknown) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        const failure = error instanceof Error ? error : new Error(String(error));
        // Syntax errors are cheap to recover from, and transport/load errors
        // must never leave a half-initialized iframe in the shared queue.
        discardRenderer(client, failure);
        reject(failure);
      },
    );
  });
}

/**
 * Mermaid configuration and parsing state live in a dedicated same-origin
 * iframe. Calls are serialized while successful renders reuse the frame;
 * timeout or failure destroys the realm before the queue admits another job.
 */
export function renderMermaid(
  source: string,
  config: MermaidConfig,
  idPrefix: string,
  isCurrent: () => boolean = () => true,
): Promise<RenderResult> {
  const execute = async (): Promise<RenderResult> => {
    if (!isCurrent()) throw new DOMException("Superseded Mermaid render", "AbortError");
    const client = currentRenderer();
    const result = await renderWithTimeout(
      client,
      client.render(
        source,
        config,
        `${idPrefix}-${crypto.randomUUID()}`,
        isCurrent,
      ),
    );
    if (!isCurrent()) throw new DOMException("Superseded Mermaid render", "AbortError");
    return result;
  };
  const result = renderTail.then(execute, execute);
  renderTail = result.then(
    () => undefined,
    () => undefined,
  );
  return result;
}

export function disposeMermaidRenderer(): void {
  const active = renderer;
  renderer = null;
  active?.destroy();
}
