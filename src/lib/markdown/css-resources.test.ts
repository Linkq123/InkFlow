import { describe, it, expect } from "vitest";
import { hasRemoteCssReference, sanitizeResourceCss } from "./css-resources";
import { blockRemoteImageRequests, hasRemoteMermaidImageReference } from "./resources";

describe("CSS image resource policy", () => {
  it("handles unfinished functions and comments without rescanning suffixes", () => {
    expect(hasRemoteCssReference("url(".repeat(30_000))).toBe(false);
    expect(hasRemoteCssReference("/*".repeat(30_000))).toBe(false);
    expect(hasRemoteCssReference('image-set("local.png" type("image/png"), "https://example.com/a.png" 2x)')).toBe(true);
  });
  it.each([
    'background-image:url("https://example.com/a.png")',
    String.raw`background-image:u\72l(https://example.com/a.png)`,
    'background-image:image-set("https://example.com/a.png" 1x)',
    '--image:url(//example.com/a.png);background-image:var(--image)',
    'cursor:url(https://example.com/a.cur),auto',
  ])("removes resource-bearing declarations: %s", declaration => {
    const safe = sanitizeResourceCss(`.label{color:red;${declaration}}`);
    expect(safe).not.toContain("example.com");
    expect(safe).toContain("color:red");
  });

  it("retains local SVG markers and removes imports and inline resource styles", () => {
    const safe = blockRemoteImageRequests('<svg><style>@import "https://example.com/a.css";.node{fill:url(#paint);color:red;background:url(https://example.com/a.png)}</style><path fill="url(https://example.com/a.svg#x)" style="stroke:blue;filter:url(//example.com/filter.svg#x)" /></svg>');
    expect(safe).not.toContain("example.com");
    expect(safe).toContain("url(#paint)");
    expect(safe).toContain("stroke:blue");
  });

  it("detects frontmatter CSS before Mermaid is called", async () => {
    const source = '---\nconfig:\n  themeCSS: |\n    .nodeLabel { background-image: url("https://example.com/a.png"); }\n---\nflowchart LR\nA';
    expect(await hasRemoteMermaidImageReference(source)).toBe(true);
  });
});
