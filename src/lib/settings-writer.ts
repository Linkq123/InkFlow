import { createLatestSerializedWriter } from "./latest-serialized-writer";

/** Preserve the UI baseline separately from concurrent server normalization. */
export function createSettingsWriter<T>(
  initial: T,
  write: (value: T, baseline: T) => Promise<T>,
  applyLatest: (value: T) => void,
) {
  let baseline = structuredClone(initial);
  const writer = createLatestSerializedWriter<T>(
    async value => {
      const normalized = await write(value, structuredClone(baseline));
      // Queued UI snapshots still contain the old values of unrelated fields.
      // Compare them with the preceding submitted snapshot, not the server reply.
      baseline = structuredClone(value);
      return normalized;
    },
    normalized => {
      baseline = structuredClone(normalized);
      applyLatest(normalized);
    },
  );
  return {
    ...writer,
    initialize(value: T): void { baseline = structuredClone(value); },
  };
}
