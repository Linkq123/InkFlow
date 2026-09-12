import { collectImageDestinationsAsync, type ImageDestination } from "./image-destinations";
import type { WorkCheckpoint } from "../async";

let worker: Worker | null = null;
let unavailable = false;
let sequence = 0;
const pending = new Map<number, {
  resolve: (images: ImageDestination[]) => void;
  reject: (error: Error) => void;
  fallback: () => void;
  timer: ReturnType<typeof setTimeout>;
}>();

function recover(): void {
  try { worker?.terminate(); } catch { /* Continue with cooperative parsing. */ }
  worker = null;
  unavailable = true;
  const requests = [...pending.values()];
  pending.clear();
  for (const request of requests) {
    clearTimeout(request.timer);
    request.fallback();
  }
}

/** History jobs share a worker without superseding requests from other tabs. */
export function parseImageDestinations(markdown: string, checkpoint: WorkCheckpoint): Promise<ImageDestination[]> {
  if (typeof Worker === "undefined" || unavailable) return collectImageDestinationsAsync(markdown, checkpoint);
  if (!worker) {
    try {
      worker = new Worker(new URL("./image-destinations.worker.ts", import.meta.url), { type: "module" });
      worker.onmessage = (event: MessageEvent<{ id: number; images?: ImageDestination[]; error?: string }>) => {
        const request = pending.get(event.data.id);
        if (!request) return;
        pending.delete(event.data.id);
        clearTimeout(request.timer);
        if (event.data.error) request.reject(new Error(event.data.error));
        else request.resolve(event.data.images ?? []);
      };
      worker.onerror = event => { event.preventDefault(); recover(); };
      worker.onmessageerror = () => recover();
    } catch {
      recover();
      return collectImageDestinationsAsync(markdown, checkpoint);
    }
  }
  const id = ++sequence;
  return new Promise((resolve, reject) => {
    pending.set(id, {
      resolve, reject,
      fallback: () => { void collectImageDestinationsAsync(markdown, checkpoint).then(resolve, reject); },
      timer: setTimeout(recover, 30_000),
    });
    try { worker!.postMessage({ id, markdown }); }
    catch { recover(); }
  });
}
