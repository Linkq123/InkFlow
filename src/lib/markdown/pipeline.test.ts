import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./pipeline";

describe("renderMarkdown", () => {
  it("renders GFM tables, task lists, footnotes and KaTeX", async () => {
    const html = await renderMarkdown([
      "| Name | Ready |",
      "| --- | --- |",
      "| InkFlow | yes |",
      "",
      "- [x] local first",
      "",
      "Inline $x^2$.[^1]",
      "",
      "[^1]: A footnote.",
    ].join("\n"));

    expect(html).toContain("<table>");
    expect(html).toContain('type="checkbox"');
    expect(html).toContain("katex");
    expect(html).toContain("footnote-ref");
  });

  it("removes scripts and event handlers from raw HTML", async () => {
    const html = await renderMarkdown(
      '<img src="x" onerror="alert(1)"><script>alert(2)</script><strong>safe</strong>',
    );

    expect(html).not.toContain("onerror");
    expect(html).not.toContain("<script");
    expect(html).toContain("<strong>safe</strong>");
  });

  it("does not expose YAML front matter as document content", async () => {
    const html = await renderMarkdown("---\ntitle: Draft\n---\n\n# Visible");
    expect(html).not.toContain("title: Draft");
    expect(html).toContain("<h1>Visible</h1>");
  });

  it("renders the compatibility fixture without executing unsafe HTML", async () => {
    const markdown = await readFile(
      resolve(process.cwd(), "tests/fixtures/markdown-compatibility.md"),
      "utf8",
    );
    const html = await renderMarkdown(markdown);

    expect(html).toContain("中文、Emoji");
    expect(html).toContain("<table>");
    expect(html).toContain("language-mermaid");
    expect(html).toContain("katex");
    expect(html).not.toContain("<script");
  });
});
