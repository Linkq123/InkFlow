export interface LatestSerializedWriter<T> {
  getRevision(): number;
  enqueue(value: T): Promise<T>;
  flush(): Promise<void>;
}

export function createLatestSerializedWriter<T>(
  write: (value: T) => Promise<T>,
  applyLatest: (value: T) => void,
): LatestSerializedWriter<T> {
  let tail: Promise<void> = Promise.resolve();
  let revision = 0;
  let latest: Promise<T> | null = null;

  return {
    getRevision: () => revision,
    enqueue(value: T): Promise<T> {
      const snapshot = structuredClone(value);
      const operationRevision = ++revision;
      const operation = tail.then(() => write(snapshot));
      latest = operation;
      tail = operation.then(
        () => undefined,
        () => undefined,
      );
      void operation.then(
        (result) => {
          if (operationRevision === revision) applyLatest(result);
        },
        () => undefined,
      );
      return operation;
    },
    async flush(): Promise<void> {
      while (latest) {
        const pending = latest;
        try {
          await pending;
        } catch (error) {
          // A newer full snapshot can persist the edits from a failed write.
          if (pending === latest) throw error;
        }
        if (pending === latest) return;
      }
    },
  };
}
