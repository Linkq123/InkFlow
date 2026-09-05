import type { OpenTargetRequest } from "./api/types";

export type { OpenTargetRequest } from "./api/types";

/** Preserves second-instance open requests exactly in arrival order. */
export class OpenTargetQueue {
  private readonly requests: OpenTargetRequest[] = [];

  get length(): number {
    return this.requests.length;
  }

  enqueuePaths(paths: readonly string[]): void {
    if (paths.length > 0) {
      this.requests.push({ kind: "paths", paths: [...paths] });
    }
  }

  enqueueWorkspace(path: string): void {
    this.requests.push({ kind: "workspace", path });
  }

  dequeue(): OpenTargetRequest | undefined {
    return this.requests.shift();
  }

  /**
   * Synchronously transfers every pending request to another queue.
   *
   * Keeping the transfer synchronous lets startup switch its listener to the
   * runtime queue in the same JavaScript turn, so no request can be stranded
   * between an awaited drain and that state change.
   */
  handoff(accept: (request: OpenTargetRequest) => void): number {
    let transferred = 0;
    while (this.requests.length > 0) {
      const request = this.requests.shift();
      if (!request) break;
      accept(request);
      transferred += 1;
    }
    return transferred;
  }
}
