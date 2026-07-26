import { describe, expect, it } from "vitest";
import { newTabForTest } from "./test-support";
import {
  activeFirstSessionTabs,
  buildSessionSnapshot,
  documentPathKey,
  isPristineStartupPlaceholder,
  orderRestoredSessionTabs,
  partitionRestoredDocuments,
  uniqueDocumentPaths,
} from "./session";

describe("session model", () => {
  it("persists saved tabs in display order without unsaved document content", () => {
    const first = newTabForTest("first", "C:\\notes\\first.md");
    const draft = newTabForTest("draft", null);
    const second = { ...newTabForTest("second", "C:\\notes\\second.md"), mode: "source" as const };
    const session = buildSessionSnapshot("C:\\notes", [first, draft, second], second.id);

    expect(session.tabs).toEqual([
      { path: "C:\\notes\\first.md", mode: "live" },
      { path: "C:\\notes\\second.md", mode: "source" },
    ]);
    expect(session.activePath).toBe("C:\\notes\\second.md");
  });

  it("restores the active path before background tabs", () => {
    const session = {
      schemaVersion: 1,
      workspaceRoot: null,
      tabs: [
        { path: "one.md", mode: "live" as const },
        { path: "two.md", mode: "preview" as const },
      ],
      activePath: "two.md",
    };
    expect(activeFirstSessionTabs(session).map((tab) => tab.path)).toEqual(["two.md", "one.md"]);
  });

  it("matches active Windows paths without case or separator sensitivity", () => {
    const session = {
      schemaVersion: 1,
      workspaceRoot: null,
      tabs: [
        { path: "C:\\notes\\one.md", mode: "live" as const },
        { path: "C:\\notes\\two.md", mode: "preview" as const },
      ],
      activePath: "c:/NOTES/TWO.md",
    };
    expect(activeFirstSessionTabs(session).map((tab) => tab.path)).toEqual([
      "C:\\notes\\two.md",
      "C:\\notes\\one.md",
    ]);
  });

  it("deduplicates startup requests against both the batch and open tabs", () => {
    const openPaths = new Set([documentPathKey("C:\\notes\\open.md")]);
    expect(uniqueDocumentPaths([
      "C:\\notes\\draft.md",
      "c:/NOTES/DRAFT.md",
      "c:/notes/open.md",
      "C:\\notes\\other.md",
    ], openPaths)).toEqual([
      "C:\\notes\\draft.md",
      "C:\\notes\\other.md",
    ]);
  });

  it("reuses a manually opened tab and rejects a duplicate restore handle", () => {
    const existing = newTabForTest("current edit", "C:\\notes\\one.md");
    const duplicate = newTabForTest("disk content", "c:/NOTES/one.md");
    const addition = newTabForTest("second", "C:\\notes\\two.md");

    const partition = partitionRestoredDocuments([existing], [duplicate, addition]);
    expect(partition.additions).toEqual([addition]);
    expect(partition.matchedExisting).toEqual([existing]);
    expect(partition.redundant).toEqual([duplicate]);
  });

  it("only replaces the untouched startup placeholder during session restore", () => {
    const placeholder = newTabForTest("", null);
    expect(isPristineStartupPlaceholder(placeholder.id, [placeholder])).toBe(true);

    const edited = {
      ...newTabForTest("draft", null),
      id: placeholder.id,
      dirty: true,
    };
    expect(isPristineStartupPlaceholder(placeholder.id, [edited])).toBe(false);
    expect(isPristineStartupPlaceholder(placeholder.id, [placeholder, edited])).toBe(false);
    expect(isPristineStartupPlaceholder("another-tab", [placeholder])).toBe(false);
  });

  it("reorders the current restored tabs without replacing edits made during startup", () => {
    const first = newTabForTest("first", "C:\\notes\\first.md");
    const second = newTabForTest("second", "C:\\notes\\second.md");
    const editedSecond = { ...second, editorVersion: 1, dirty: true };
    const draft = newTabForTest("draft", null);
    const session = {
      schemaVersion: 1,
      workspaceRoot: null,
      tabs: [
        { path: first.path!, mode: "live" as const },
        { path: second.path!, mode: "live" as const },
      ],
      activePath: second.path,
    };

    const ordered = orderRestoredSessionTabs(
      session,
      [editedSecond, draft, first],
      new Set([first.id, second.id]),
    );
    expect(ordered).toEqual([first, editedSecond, draft]);
    expect(ordered[1]).toBe(editedSecond);
  });
});
