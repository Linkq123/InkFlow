export class CheckpointWarningThrottle {
  private readonly shown = new Map<string, Map<string, number>>();

  constructor(private readonly intervalMs: number) {}

  shouldShow(documentId: string, code: string, now = Date.now()): boolean {
    let documentWarnings = this.shown.get(documentId);
    if (!documentWarnings) {
      documentWarnings = new Map<string, number>();
      this.shown.set(documentId, documentWarnings);
    }
    const previous = documentWarnings.get(code);
    const elapsed = previous === undefined ? null : now - previous;
    if (elapsed !== null && elapsed >= 0 && elapsed < this.intervalMs) {
      return false;
    }
    documentWarnings.set(code, now);
    return true;
  }

  reset(documentId: string): void {
    this.shown.delete(documentId);
  }
}
