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

  it("preserves sanitized responsive image sources for scoped export handling", async () => {
    const html = await renderMarkdown(
      '<picture><source media="(min-width: 48rem)" type="image/webp" sizes="50vw" srcset="wide.webp 2x"><img src="fallback.png" sizes="100vw" srcset="small.png 1x, large.png 2x" alt="responsive"></picture>',
    );
    const documentNode = new DOMParser().parseFromString(html, "text/html");
    const source = documentNode.querySelector("source");
    const image = documentNode.querySelector("img");

    expect(source?.getAttribute("srcset")).toBe("wide.webp 2x");
    expect(source?.getAttribute("media")).toBe("(min-width: 48rem)");
    expect(source?.getAttribute("type")).toBe("image/webp");
    expect(source?.getAttribute("sizes")).toBe("50vw");
    expect(image?.getAttribute("srcset"))
      .toBe("small.png 1x, large.png 2x");
    expect(image?.getAttribute("sizes")).toBe("100vw");
    expect(html).not.toContain("onerror");
  });

  it("removes disallowed protocols from every responsive image candidate", async () => {
    const html = await renderMarkdown([
      '<picture><source srcset="local.webp 1x, file:///C:/private.webp 2x, ftp://example.com/image.webp 3x, https://example.com/image.webp 4x">',
      '<img srcset="custom:image.png 1x, local.png 2x, https://example.com/image.png 3x" alt="responsive"></picture>',
    ].join(""));
    const documentNode = new DOMParser().parseFromString(html, "text/html");

    expect(documentNode.querySelector("source")?.getAttribute("srcset"))
      .toBe("local.webp 1x, https://example.com/image.webp 4x");
    expect(documentNode.querySelector("img")?.getAttribute("srcset"))
      .toBe("local.png 2x, https://example.com/image.png 3x");
    expect(html).not.toContain("file:");
    expect(html).not.toContain("ftp:");
    expect(html).not.toContain("custom:");
  });

  it("drops srcset when every candidate uses a disallowed protocol", async () => {
    const html = await renderMarkdown(
      '<picture><source srcset="file:///C:/private.webp 1x"><img srcset="ftp://example.com/image.png 2x" alt="responsive"></picture>',
    );
    const documentNode = new DOMParser().parseFromString(html, "text/html");

    expect(documentNode.querySelector("source")?.hasAttribute("srcset"))
      .toBe(false);
    expect(documentNode.querySelector("img")?.hasAttribute("srcset"))
      .toBe(false);
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
