export type ValueMutation<T> = (value: T) => T;

export interface HydrationResult<T> {
  value: T;
  shouldPersist: boolean;
}

export interface DeferredHydration<T> {
  apply(current: T, mutation: ValueMutation<T>): T;
  requestPersistence(): boolean;
  hydrate(loaded: T): HydrationResult<T>;
  completeWithCurrent(current: T): HydrationResult<T>;
}

export function createDeferredHydration<T>(
  initiallyHydrated = false,
): DeferredHydration<T> {
  let hydrated = initiallyHydrated;
  let persistenceRequested = false;
  let mutations: ValueMutation<T>[] = [];

  return {
    apply(current: T, mutation: ValueMutation<T>): T {
      if (!hydrated) mutations.push(mutation);
      return mutation(current);
    },

    requestPersistence(): boolean {
      if (hydrated) return true;
      persistenceRequested = true;
      return false;
    },

    hydrate(loaded: T): HydrationResult<T> {
      if (hydrated) {
        return { value: loaded, shouldPersist: false };
      }
      const value = mutations.reduce(
        (current, mutation) => mutation(current),
        loaded,
      );
      const shouldPersist = persistenceRequested;
      mutations = [];
      persistenceRequested = false;
      hydrated = true;
      return { value, shouldPersist };
    },

    completeWithCurrent(current: T): HydrationResult<T> {
      if (hydrated) {
        return { value: current, shouldPersist: false };
      }
      const shouldPersist = persistenceRequested;
      mutations = [];
      persistenceRequested = false;
      hydrated = true;
      return { value: current, shouldPersist };
    },
  };
}
