import { hasRemoteImages } from "./resources";
import { documentStats, extractOutline, type DocumentStats, type OutlineItem } from "../stats";

let worker: Worker | null = null;
let revision = 0;
let workerUnavailable = false;

type WorkerOperation = "render" | "detectRemoteImages" | "analyze";

export interface MarkdownAnalysis {
  stats: DocumentStats;
  outline: OutlineItem[];
  hasRemoteImages: boolean;
}

type WorkerResult = string | boolean | MarkdownAnalysis;
const MAX_MAIN_THREAD_ANALYSIS_CHARACTERS = 512 * 1024;

const pending = new Map<number, {
  markdown: string;
  operation: WorkerOperation;
  resolve: (value: WorkerResult) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}>();

async function runOnMainThread(
  operation: WorkerOperation,
  markdown: string,
): Promise<WorkerResult> {
  if (operation === "detectRemoteImages") {
    return hasRemoteImages(markdown);
  }
  if (operation === "analyze") {
    if (markdown.length > MAX_MAIN_THREAD_ANALYSIS_CHARACTERS) {
      throw new Error("Large-document analysis was skipped because the Markdown worker is unavailable.");
    }
    return {
      stats: documentStats(markdown),
      outline: extractOutline(markdown),
      hasRemoteImages: await hasRemoteImages(markdown),
    };
  }
  const { renderMarkdown } = await import("./pipeline");
  return renderMarkdown(markdown);
}

function recoverPendingRequests(): void {
  const requests = [...pending.values()];
  pending.clear();
  worker?.terminate();
  worker = null;
  workerUnavailable = true;
  for (const request of requests) {
    clearTimeout(request.timer);
    void runOnMainThread(request.operation, request.markdown)
      .then(request.resolve, request.reject);
  }
}

function restartWorker(): Worker | null {
  worker?.terminate();
  worker = null;
  const replacement = getWorker();
  if (!replacement) return null;
  try {
    for (const [requestRevision, request] of pending) {
      replacement.postMessage({
        revision: requestRevision,
        markdown: request.markdown,
        operation: request.operation,
      });
    }
  } catch {
    recoverPendingRequests();
    return null;
  }
  return replacement;
}

function getWorker(): Worker | null {
  if (typeof Worker === "undefined" || workerUnavailable) return null;
  if (!worker) {
    worker = new Worker(new URL("./renderer.worker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (event: MessageEvent<{
      revision: number;
      html?: string;
      hasRemoteImages?: boolean;
      analysis?: MarkdownAnalysis;
      error?: string;
    }>) => {
      const request = pending.get(event.data.revision);
      if (!request) return;
      pending.delete(event.data.revision);
      clearTimeout(request.timer);
      if (event.data.error) request.reject(new Error(event.data.error));
      else if (request.operation === "detectRemoteImages") {
        request.resolve(event.data.hasRemoteImages ?? false);
      } else if (request.operation === "analyze") {
        request.resolve(event.data.analysis ?? {
          stats: documentStats(""),
          outline: [],
          hasRemoteImages: false,
        });
      } else {
        request.resolve(event.data.html ?? "");
      }
    };
    worker.onerror = (event) => {
      event.preventDefault();
      recoverPendingRequests();
    };
  }
  return worker;
}

function requestWorker(markdown: string, operation: "render"): Promise<string>;
function requestWorker(
  markdown: string,
  operation: "detectRemoteImages",
): Promise<boolean>;
function requestWorker(
  markdown: string,
  operation: "analyze",
): Promise<MarkdownAnalysis>;
function requestWorker(
  markdown: string,
  operation: WorkerOperation,
): Promise<WorkerResult> {
  let activeWorker = getWorker();
  if (!activeWorker) {
    return runOnMainThread(operation, markdown);
  }
  const current = ++revision;
  let superseded = false;
  for (const [id, request] of pending) {
    if (id >= current || request.operation !== operation) continue;
    clearTimeout(request.timer);
    request.reject(new DOMException("Superseded by a newer request", "AbortError"));
    pending.delete(id);
    superseded = true;
  }
  if (superseded) {
    activeWorker = restartWorker();
    if (!activeWorker) return runOnMainThread(operation, markdown);
  }
  return new Promise<WorkerResult>((resolve, reject) => {
    const timer = setTimeout(() => {
      if (!pending.delete(current)) return;
      reject(new Error(`${operation} timed out.`));
      restartWorker();
    }, 15_000);
    pending.set(current, { markdown, operation, resolve, reject, timer });
    try {
      activeWorker.postMessage({ revision: current, markdown, operation });
    } catch {
      recoverPendingRequests();
    }
  });
}

export function renderInWorker(markdown: string): Promise<string> {
  return requestWorker(markdown, "render");
}

export function detectRemoteImagesInWorker(
  markdown: string,
): Promise<boolean> {
  return requestWorker(markdown, "detectRemoteImages");
}

export function analyzeMarkdownInWorker(markdown: string): Promise<MarkdownAnalysis> {
  return requestWorker(markdown, "analyze");
}
