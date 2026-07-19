let worker: Worker | null = null;
let revision = 0;
let workerUnavailable = false;
const pending = new Map<number, {
  markdown: string;
  resolve: (html: string) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}>();

function renderOnMainThread(markdown: string): Promise<string> {
  return import("./pipeline").then(({ renderMarkdown }) => renderMarkdown(markdown));
}

function recoverPendingRenders(): void {
  const requests = [...pending.values()];
  pending.clear();
  worker?.terminate();
  worker = null;
  workerUnavailable = true;
  for (const request of requests) {
    clearTimeout(request.timer);
    void renderOnMainThread(request.markdown).then(request.resolve, request.reject);
  }
}

function getWorker(): Worker | null {
  if (typeof Worker === "undefined" || workerUnavailable) return null;
  if (!worker) {
    worker = new Worker(new URL("./renderer.worker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (event: MessageEvent<{ revision: number; html?: string; error?: string }>) => {
      const request = pending.get(event.data.revision);
      if (!request) return;
      pending.delete(event.data.revision);
      clearTimeout(request.timer);
      if (event.data.error) request.reject(new Error(event.data.error));
      else request.resolve(event.data.html ?? "");
    };
    worker.onerror = (event) => {
      event.preventDefault();
      recoverPendingRenders();
    };
  }
  return worker;
}

export function renderInWorker(markdown: string): Promise<string> {
  const activeWorker = getWorker();
  if (!activeWorker) {
    return renderOnMainThread(markdown);
  }
  const current = ++revision;
  for (const [id, request] of pending) {
    if (id < current) {
      clearTimeout(request.timer);
      request.reject(new DOMException("Superseded by a newer render", "AbortError"));
      pending.delete(id);
    }
  }
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      if (!pending.delete(current)) return;
      reject(new Error("Markdown rendering timed out."));
    }, 15_000);
    pending.set(current, { markdown, resolve, reject, timer });
    try {
      activeWorker.postMessage({ revision: current, markdown });
    } catch {
      recoverPendingRenders();
    }
  });
}
