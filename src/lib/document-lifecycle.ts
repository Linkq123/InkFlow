/** Parallel opens may install together; close/rename form a barrier around registration. */
export class DocumentLifecycle {
  private readonly opens = new Set<Promise<unknown>>();
  private closeTail: Promise<unknown> = Promise.resolve();

  open<T>(operation: () => Promise<T>): Promise<T> {
    const pending = this.closeTail.then(operation);
    this.opens.add(pending);
    const cleanup = () => this.opens.delete(pending);
    void pending.then(cleanup, cleanup);
    return pending;
  }

  close<T>(operation: () => Promise<T>): Promise<T> {
    const pending = Promise.allSettled([this.closeTail, ...this.opens]).then(operation);
    // A failed close must not prevent subsequent operations from running.
    this.closeTail = pending.then(() => undefined, () => undefined);
    return pending;
  }
}
