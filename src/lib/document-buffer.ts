import type { Text } from "@codemirror/state";

export interface VersionedDocumentBuffer {
  id: string;
  editorVersion: number;
  content: Text;
}

export class DocumentSerializer {
  private cache = new Map<string, { version: number; content: string }>();

  constructor(private readonly maxEntries = 4) {}

  serialize(document: VersionedDocumentBuffer): string {
    const cached = this.cache.get(document.id);
    if (cached?.version === document.editorVersion) {
      this.cache.delete(document.id);
      this.cache.set(document.id, cached);
      return cached.content;
    }
    const content = document.content.toString();
    this.cache.delete(document.id);
    this.cache.set(document.id, { version: document.editorVersion, content });
    while (this.cache.size > Math.max(1, this.maxEntries)) {
      const oldest = this.cache.keys().next().value;
      if (oldest === undefined) break;
      this.cache.delete(oldest);
    }
    return content;
  }

  invalidate(documentId: string): void {
    this.cache.delete(documentId);
  }

  clear(): void {
    this.cache.clear();
  }
}
