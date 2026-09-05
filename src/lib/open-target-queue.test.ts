import { describe, expect, it } from "vitest";
import { OpenTargetQueue } from "./open-target-queue";

describe("OpenTargetQueue", () => {
  it("keeps file and workspace requests in arrival order without collapsing workspaces", () => {
    const queue = new OpenTargetQueue();

    queue.enqueuePaths(["first.md", "second.md"]);
    queue.enqueueWorkspace("C:\\first-workspace");
    queue.enqueueWorkspace("C:\\second-workspace");
    queue.enqueuePaths(["last.md"]);

    expect(queue.length).toBe(4);
    expect(queue.dequeue()).toEqual({
      kind: "paths",
      paths: ["first.md", "second.md"],
    });
    expect(queue.dequeue()).toEqual({
      kind: "workspace",
      path: "C:\\first-workspace",
    });
    expect(queue.dequeue()).toEqual({
      kind: "workspace",
      path: "C:\\second-workspace",
    });
    expect(queue.dequeue()).toEqual({ kind: "paths", paths: ["last.md"] });
    expect(queue.dequeue()).toBeUndefined();
  });

  it("does not enqueue an empty file request", () => {
    const queue = new OpenTargetQueue();
    queue.enqueuePaths([]);
    expect(queue.length).toBe(0);
  });

  it("hands every startup request to the runtime queue synchronously and in order", () => {
    const queue = new OpenTargetQueue();
    const runtime: ReturnType<OpenTargetQueue["dequeue"]>[] = [];
    queue.enqueuePaths(["first.md"]);
    queue.enqueueWorkspace("C:\\workspace");
    queue.enqueuePaths(["last.md"]);

    const transferred = queue.handoff((request) => runtime.push(request));

    expect(transferred).toBe(3);
    expect(queue.length).toBe(0);
    expect(runtime).toEqual([
      { kind: "paths", paths: ["first.md"] },
      { kind: "workspace", path: "C:\\workspace" },
      { kind: "paths", paths: ["last.md"] },
    ]);
  });
});
