import { Text } from "@codemirror/state";
import { describe, expect, it, vi } from "vitest";
import { DocumentSerializer } from "./document-buffer";

describe("document serializer", () => {
  it("materializes a document only once for each editor version", () => {
    const serializer = new DocumentSerializer();
    const toString = vi.spyOn(Text.prototype, "toString");
    const document = {
      id: "doc",
      editorVersion: 1,
      content: Text.of(["large", "document"]),
    };

    expect(serializer.serialize(document)).toBe("large\ndocument");
    expect(serializer.serialize(document)).toBe("large\ndocument");
    expect(toString).toHaveBeenCalledTimes(1);

    serializer.invalidate(document.id);
    serializer.serialize({ ...document, editorVersion: 2, content: document.content.append(Text.of(["more"])) });
    expect(toString).toHaveBeenCalledTimes(2);
    toString.mockRestore();
  });

  it("does not serialize when an editor publishes a persistent Text update", () => {
    const serializer = new DocumentSerializer();
    const toString = vi.spyOn(Text.prototype, "toString");
    const current = Text.of(["before"]);
    const updated = current.replace(current.length, current.length, Text.of([" after"]));

    expect(updated.length).toBeGreaterThan(current.length);
    expect(toString).not.toHaveBeenCalled();
    serializer.serialize({ id: "doc", editorVersion: 1, content: updated });
    expect(toString).toHaveBeenCalledTimes(1);
    toString.mockRestore();
  });

  it("bounds cached full-document strings and retains the most recently used entry", () => {
    const serializer = new DocumentSerializer(2);
    const documents = ["one", "two", "three"].map((id) => ({
      id,
      editorVersion: 1,
      content: Text.of([id]),
    }));
    const toString = vi.spyOn(Text.prototype, "toString");

    serializer.serialize(documents[0]);
    serializer.serialize(documents[1]);
    serializer.serialize(documents[0]);
    serializer.serialize(documents[2]);
    serializer.serialize(documents[0]);
    serializer.serialize(documents[1]);

    expect(toString).toHaveBeenCalledTimes(4);
    toString.mockRestore();
  });
});
