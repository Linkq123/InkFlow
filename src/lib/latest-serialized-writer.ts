export interface LatestSerializedWriter<T> {
  getRevision(): number;
  enqueue(value: T): Promise<T>;
}

export function createLatestSerializedWriter<T>(
  write: (value: T) => Promise<T>,
  applyLatest: (value: T) => void,
): LatestSerializedWriter<T> {
  let tail: Promise<void> = Promise.resolve();
  let revision = 0;

  return {
    getRevision: () => revision,
    enqueue(value: T): Promise<T> {
      const snapshot = structuredClone(value);
      const operationRevision = ++revision;
      const operation = tail.then(() => write(snapshot));
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
  };
}
