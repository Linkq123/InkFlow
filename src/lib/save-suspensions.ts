/** Each mutation releases only the save suspension it acquired. */
export class SaveSuspensions {
  private readonly owners = new Map<string, Set<symbol>>();

  has(id: string): boolean {
    return !!this.owners.get(id)?.size;
  }

  acquire(ids: Iterable<string>): () => void {
    const documents = new Set(ids);
    const owner = Symbol();
    for (const id of documents) {
      const owners = this.owners.get(id) ?? new Set<symbol>();
      owners.add(owner);
      this.owners.set(id, owners);
    }
    return () => {
      for (const id of documents) {
        const owners = this.owners.get(id);
        owners?.delete(owner);
        if (owners?.size === 0) this.owners.delete(id);
      }
    };
  }
}
