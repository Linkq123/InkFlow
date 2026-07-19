import { describe, expect, it } from "vitest";
import { documentStats, extractOutline } from "./stats";

describe("documentStats", () => {
  it("counts CJK characters and Latin words without counting UTF-16 code units", () => {
    expect(documentStats("你好 InkFlow editor 😀")).toEqual({
      words: 4,
      lines: 1,
      characters: 19,
    });
  });

  it("keeps an empty document as one visible line", () => {
    expect(documentStats("").lines).toBe(1);
  });
});

describe("extractOutline", () => {
  it("extracts ATX headings and ignores headings inside fences", () => {
    const outline = extractOutline([
      "# InkFlow",
      "```md",
      "## not an outline item",
      "```",
      "### 编辑体验 ###",
    ].join("\n"));

    expect(outline).toEqual([
      { level: 1, text: "InkFlow", line: 1 },
      { level: 3, text: "编辑体验", line: 5 },
    ]);
  });
});
