import { describe, expect, it } from "vitest";
import { DocumentLifecycle } from "./document-lifecycle";

describe("document registration lifecycle", () => {
  it("allows parallel opens but waits for installation before closing and reopening", async () => {
    const lifecycle = new DocumentLifecycle();
    const events: string[] = [];
    let install!: () => void;
    let closed!: () => void;
    const first = lifecycle.open(async () => {
      events.push("read");
      await new Promise<void>(resolve => install = resolve);
      events.push("install");
    });
    const parallel = lifecycle.open(async () => { events.push("parallel"); });
    const close = lifecycle.close(async () => {
      events.push("close");
      await new Promise<void>(resolve => closed = resolve);
      events.push("unregister");
    });
    const reopened = lifecycle.open(async () => { events.push("reopen"); });
    await parallel;
    expect(events).toEqual(["read", "parallel"]);
    install();
    await first;
    // Wait until the close callback has started without releasing its IPC.
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(events).toEqual(["read", "parallel", "install", "close"]);
    closed();
    await Promise.all([close, reopened]);
    expect(events).toEqual(["read", "parallel", "install", "close", "unregister", "reopen"]);
  });

  it("does not strand later operations when opening or closing fails", async () => {
    const lifecycle = new DocumentLifecycle();
    const open = lifecycle.open(async () => { throw new Error("read failed"); });
    const close = lifecycle.close(async () => { throw new Error("close failed"); });
    const next = lifecycle.open(async () => "opened");
    await expect(open).rejects.toThrow("read failed");
    await expect(close).rejects.toThrow("close failed");
    await expect(next).resolves.toBe("opened");
  });
});
