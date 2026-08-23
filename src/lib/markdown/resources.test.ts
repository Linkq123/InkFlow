import { describe, expect, it, vi } from "vitest";
import { renderMarkdown } from "./pipeline";
import {
  blockRemoteImageRequests,
  decodeMarkdownResourceDestination,
  hasRemoteImages,
  hasRemoteMermaidImageReference,
  hasRetainedResponsiveImageSource,
  hasUsableResponsiveImageSource,
  isRemoteImageSource,
} from "./resources";

describe("Markdown image resources", () => {
  it("detects inline, angle, reference, and HTML remote images", async () => {
    expect(await hasRemoteImages("![a](https://example.com/a.png)")).toBe(true);
    expect(await hasRemoteImages("![a](<https://example.com/a.png>)")).toBe(true);
    expect(await hasRemoteImages("![a](https:/example.com/a.png)")).toBe(true);
    expect(await hasRemoteImages("![a](https:example.com/a.png)")).toBe(true);
    expect(await hasRemoteImages("![a](//example.com/a.png)")).toBe(true);
    expect(await hasRemoteImages("![a](https&#58;//example.com/a.png)")).toBe(true);
    expect(await hasRemoteImages("![a][asset]\n\n[asset]: https://example.com/a.png")).toBe(true);
    expect(await hasRemoteImages("![a][asset]\n\n[asset]: https:/example.com/a.png")).toBe(true);
    expect(await hasRemoteImages("![a][asset]\n\n[asset]: https&#58;//example.com/a.png")).toBe(true);
    expect(await hasRemoteImages('<img src="https://example.com/a.png">')).toBe(true);
    expect(await hasRemoteImages('<img alt=">" src="https://example.com/a.png">')).toBe(true);
    expect(await hasRemoteImages('<img src="https&#58;//example.com/a.png">')).toBe(true);
    expect(await hasRemoteImages("<img src=https:example.com>")).toBe(true);
    expect(await hasRemoteImages('<picture><source srcset="local.png 1x, //example.com/a.png 2x"><img src="local.png"></picture>')).toBe(true);
    expect(await hasRemoteImages('<img data-src="https://example.com/lazy.png">')).toBe(false);
    expect(await hasRemoteImages("[site]: https://example.com\n\n[site]")).toBe(false);
  });

  it("detects a remote image whose CommonMark alt text spans lines", async () => {
    const markdown = "![first line\nsecond line](https://example.com/a.png)";

    expect(await renderMarkdown(markdown)).toContain('src="https://example.com/a.png"');
    expect(await hasRemoteImages(markdown)).toBe(true);
  });

  it("detects remote images with escaped alt punctuation", async () => {
    const escapedAlt = String.raw`![a\]](https://example.com/a.png)`;

    expect(await renderMarkdown(escapedAlt)).toContain('src="https://example.com/a.png"');
    expect(await hasRemoteImages(escapedAlt)).toBe(true);
  });

  it("detects remote images whose alt text contains balanced brackets", async () => {
    const nestedAlt = "![a [nested]](https://example.com/a.png)";
    const balancedDestination =
      "![a [nested]](https://example.com/image_(large).png)";
    const unmatchedOuter = "![outer ![inner](https://example.com/inner.png)";
    const nestedReference = [
      "![a [nested]][asset]",
      "",
      "[asset]: https://example.com/reference.png",
    ].join("\n");

    for (
      const markdown of [
        nestedAlt,
        balancedDestination,
        unmatchedOuter,
        nestedReference,
      ]
    ) {
      expect(await renderMarkdown(markdown)).toContain('src="https://example.com/');
      expect(await hasRemoteImages(markdown)).toBe(true);
    }
  });

  it("detects reference images whose labels contain escaped punctuation", async () => {
    const escapedReference = [
      String.raw`![a][asset\]]`,
      "",
      String.raw`[asset\]]: https://example.com/a.png`,
    ].join("\n");

    expect(await renderMarkdown(escapedReference)).toContain('src="https://example.com/a.png"');
    expect(await hasRemoteImages(escapedReference)).toBe(true);
  });

  it("detects destinations whose remote scheme uses CommonMark escapes", async () => {
    const escapedInline = String.raw`![a](https\://example.com/a.png)`;
    const escapedReference = [
      "![a][asset]",
      "",
      String.raw`[asset]: https\://example.com/a.png`,
    ].join("\n");

    for (const markdown of [escapedInline, escapedReference]) {
      expect(await renderMarkdown(markdown)).toContain('src="https://example.com/a.png"');
      expect(await hasRemoteImages(markdown)).toBe(true);
    }
  });

  it("uses container definitions and only the first definition for a label", async () => {
    const blockQuoteDefinition = [
      "> ![a][asset]",
      ">",
      "> [asset]: https://example.com/a.png",
    ].join("\n");
    const listDefinition = [
      "- ![a][asset]",
      "",
      "- [asset]: https://example.com/a.png",
    ].join("\n");
    const localFirst = [
      "![a][asset]",
      "",
      "[asset]: local.png",
      "[asset]: https://example.com/a.png",
    ].join("\n");
    const indentedListDefinition = [
      "- ![a][asset]",
      "",
      "    [asset]: https://example.com/a.png",
    ].join("\n");
    const nestedListDefinition = [
      "- outer",
      "  - ![a][asset]",
      "",
      "    [asset]: https://example.com/a.png",
    ].join("\n");

    expect(await renderMarkdown(blockQuoteDefinition))
      .toContain('src="https://example.com/a.png"');
    expect(await hasRemoteImages(blockQuoteDefinition)).toBe(true);
    expect(await renderMarkdown(listDefinition))
      .toContain('src="https://example.com/a.png"');
    expect(await hasRemoteImages(listDefinition)).toBe(true);
    expect(await renderMarkdown(localFirst)).toContain('src="local.png"');
    expect(await renderMarkdown(localFirst)).not.toContain('src="https://example.com/a.png"');
    expect(await hasRemoteImages(localFirst)).toBe(false);
    for (const markdown of [indentedListDefinition, nestedListDefinition]) {
      expect(await renderMarkdown(markdown))
        .toContain('src="https://example.com/a.png"');
      expect(await hasRemoteImages(markdown)).toBe(true);
    }
  });

  it("detects a reference destination on the line after its label", async () => {
    const multilineDefinition = [
      "![a][asset]",
      "",
      "[asset]:",
      "  https://example.com/a.png",
    ].join("\n");
    const localFirst = [
      "![a][asset]",
      "",
      "[asset]:",
      "  local.png",
      "[asset]: https://example.com/a.png",
    ].join("\n");
    const listDefinition = [
      "- ![a][asset]",
      "",
      "- [asset]:",
      "  https://example.com/a.png",
    ].join("\n");

    expect(await renderMarkdown(multilineDefinition))
      .toContain('src="https://example.com/a.png"');
    expect(await hasRemoteImages(multilineDefinition)).toBe(true);
    expect(await renderMarkdown(listDefinition))
      .toContain('src="https://example.com/a.png"');
    expect(await hasRemoteImages(listDefinition)).toBe(true);
    expect(await renderMarkdown(localFirst)).toContain('src="local.png"');
    expect(await hasRemoteImages(localFirst)).toBe(false);
  });

  it("ignores remote image examples that CommonMark does not render as images", async () => {
    const examples = [
      String.raw`\![escaped](https://example.com/escaped.png)`,
      "![missing-close](https://example.com/missing-close.png",
      "![missing-angle-close](<https://example.com/missing-angle-close.png>",
      '![unterminated-title](https://example.com/title.png "unterminated)',
      '![blank-line](https://example.com/a.png\n\n"title")',
      "![paragraph-reference][asset]\n[asset]: https://example.com/a.png",
      [
        "![reference][asset]",
        "",
        '[asset]: https://example.com/reference.png "unterminated',
      ].join("\n"),
      "`![inline](https://example.com/inline.png)`",
      [
        "```markdown",
        "![fenced](https://example.com/fenced.png)",
        "```",
      ].join("\n"),
      [
        "- ```markdown",
        "  ![listed](https://example.com/listed.png)",
        "  ```",
      ].join("\n"),
      [
        "10. ```markdown",
        "    ![ordered](https://example.com/ordered.png)",
        "    ```",
      ].join("\n"),
      [
        "> ```markdown",
        "> ![quoted](https://example.com/quoted.png)",
        "> ```",
      ].join("\n"),
      [
        "    ![indented](https://example.com/indented.png)",
      ].join("\n"),
    ];

    for (const markdown of examples) {
      expect(await renderMarkdown(markdown)).not.toContain("<img");
      expect(await hasRemoteImages(markdown)).toBe(false);
    }
  });

  it("does not hide an image after a list-contained fence has ended", async () => {
    const markdown = [
      "- ```markdown",
      "  example",
      "  ```",
      "",
      "![actual](https://example.com/actual.png)",
    ].join("\n");

    expect(await renderMarkdown(markdown)).toContain('src="https://example.com/actual.png"');
    expect(await hasRemoteImages(markdown)).toBe(true);
  });

  it("still detects an image after an even number of backslashes", async () => {
    const markdown = String.raw`\\![image](https://example.com/a.png)`;

    expect(await renderMarkdown(markdown)).toContain('src="https://example.com/a.png"');
    expect(await hasRemoteImages(markdown)).toBe(true);
  });

  it("recognizes protocol-relative and browser-normalized network images", () => {
    expect(isRemoteImageSource("//example.com/a.png")).toBe(true);
    expect(isRemoteImageSource("https:/example.com/a.png")).toBe(true);
    expect(isRemoteImageSource("https:example.com/a.png")).toBe(true);
    expect(isRemoteImageSource(String.raw`https:\\example.com\a.png`)).toBe(true);
    expect(isRemoteImageSource("https:\t//example.com/a.png")).toBe(true);
    expect(isRemoteImageSource(String.raw`\\example.com\a.png`)).toBe(true);
    expect(isRemoteImageSource(`\u0001https://example.com/a.png`)).toBe(true);
    expect(isRemoteImageSource(`\u001f//example.com/a.png`)).toBe(true);
    expect(isRemoteImageSource("assets/a.png")).toBe(false);
    expect(isRemoteImageSource(String.raw`assets\a.png`)).toBe(false);
  });

  it("skips full rendering when no image syntax is present", async () => {
    expect(await hasRemoteImages("# Plain document\n\n[site](https://example.com)"))
      .toBe(false);
  });

  it("detects remote Mermaid images without treating ordinary Mermaid links as images", async () => {
    const remoteImage = [
      "```mermaid",
      "flowchart LR",
      'A@{ img: "https://example.com/a.png", label: "A" }',
      "```",
    ].join("\n");
    const escapedRemoteImage = [
      "```mermaid",
      "flowchart LR",
      'A@{ "img": "\\u0068ttps://example.com/a.png" }',
      "```",
    ].join("\n");
    const remoteSequenceIcon = [
      "```mermaid",
      "sequenceDiagram",
      'participant A@{ icon: "https://example.com/a.png" }',
      "```",
    ].join("\n");
    const remoteMarkdownImage = [
      "flowchart LR",
      'A["![preview](//example.com/a.png)"]',
    ].join("\n");

    expect(await hasRemoteMermaidImageReference('A@{ img: "https:\\\\example.com\\a.png" }'))
      .toBe(true);
    expect(await hasRemoteMermaidImageReference('A@{ "img": "\\u0068ttps://example.com/a.png" }'))
      .toBe(true);
    expect(await hasRemoteMermaidImageReference(
      'A@{ source: &remote "https://example.com/a.png", img: *remote }',
    )).toBe(true);
    expect(await hasRemoteMermaidImageReference(
      'participant A@{ icon: "https://example.com/a.png" }',
    )).toBe(true);
    expect(await hasRemoteMermaidImageReference(remoteMarkdownImage)).toBe(true);
    expect(await hasRemoteMermaidImageReference('click A "https://example.com"')).toBe(false);
    expect(await hasRemoteMermaidImageReference('A@{ img: "assets/a.png" }')).toBe(false);
    expect(await hasRemoteMermaidImageReference('participant A@{ icon: "@mdi/account" }'))
      .toBe(false);
    expect(await hasRemoteImages(remoteImage)).toBe(true);
    expect(await hasRemoteImages(escapedRemoteImage)).toBe(true);
    expect(await hasRemoteImages(remoteSequenceIcon)).toBe(true);
  });

  it("detects Mermaid images hidden by YAML parsing rules", async () => {
    const lineContinuation = [
      "flowchart LR",
      "A@{",
      '  img: "ht\\',
      '    tps://example.com/a.png"',
      "}",
    ].join("\n");
    const escapedKey = String.raw`A@{ "\u0069mg": "https://example.com/a.png" }`;

    expect(await hasRemoteMermaidImageReference(lineContinuation)).toBe(true);
    expect(await hasRemoteMermaidImageReference(escapedKey)).toBe(true);
    expect(await hasRemoteImages([
      "```mermaid",
      lineContinuation,
      "```",
    ].join("\n"))).toBe(true);
  });

  it("uses Mermaid's block-mapping rules for multiline image metadata", async () => {
    const localImage = [
      "flowchart LR",
      "A@{",
      '  img: "assets/a.png"',
      '  label: "Local image"',
      "}",
    ].join("\n");
    const remoteImage = localImage.replace(
      "assets/a.png",
      "https://example.com/a.png",
    );

    expect(await hasRemoteMermaidImageReference(localImage)).toBe(false);
    expect(await hasRemoteMermaidImageReference(remoteImage)).toBe(true);
    expect(await hasRemoteImages([
      "```mermaid",
      localImage,
      "```",
    ].join("\n"))).toBe(false);
  });

  it("decodes CommonMark character references in resource destinations", async () => {
    expect(decodeMarkdownResourceDestination("https&#58;//example.com/a.png"))
      .toBe("https://example.com/a.png");
    expect(decodeMarkdownResourceDestination(String.raw`https\://example.com/a.png`))
      .toBe("https://example.com/a.png");
    expect(decodeMarkdownResourceDestination("assets/a&amp;b.png"))
      .toBe("assets/a&b.png");
    expect(decodeMarkdownResourceDestination("https&#58//example.com/a.png"))
      .toBe("https&#58//example.com/a.png");
    expect(await hasRemoteImages("![a](https&#58//example.com/a.png)")).toBe(false);
  });

  it("moves remote sources to inert data attributes before DOM insertion", () => {
    const html = blockRemoteImageRequests('<p><img src="https://example.com/a.png" alt="a"><img src="local.png"></p>');
    expect(html).not.toMatch(/(?:^|\s)src=["']https?:\/\//i);
    expect(html).toContain('data-inkflow-remote-src="https://example.com/a.png"');
    expect(html).toContain('src="local.png"');
    expect(html).toContain("remote-blocked");
  });

  it("neutralizes remote image references in generated SVG", () => {
    const html = blockRemoteImageRequests(
      [
        '<svg xmlns:xlink="http://www.w3.org/1999/xlink">',
        '<image href="https://example.com/a.png"></image>',
        '<image xlink:href="https://example.com/legacy.png"></image>',
        '<use xlink:href="https://example.com/icons.svg#account"></use>',
        '<image href="data:image/png;base64,AA=="></image>',
        "</svg>",
      ].join(""),
    );
    const template = document.createElement("template");
    template.innerHTML = html;
    const images = template.content.querySelectorAll("image");
    const use = template.content.querySelector("use");

    expect(images[0].hasAttribute("href")).toBe(false);
    expect(images[0].getAttribute("data-inkflow-remote-href"))
      .toBe("https://example.com/a.png");
    expect(images[1].hasAttribute("xlink:href")).toBe(false);
    expect(images[1].getAttribute("data-inkflow-remote-xlink-href"))
      .toBe("https://example.com/legacy.png");
    expect(use?.hasAttribute("xlink:href")).toBe(false);
    expect(use?.getAttribute("data-inkflow-remote-xlink-href"))
      .toBe("https://example.com/icons.svg#account");
    expect(images[2].getAttribute("href")).toBe("data:image/png;base64,AA==");
  });

  it("neutralizes network sources hidden behind URL control characters", () => {
    const source = `\u0001https://example.com/a.png`;
    const html = blockRemoteImageRequests(`<img src="${source}">`);
    const template = document.createElement("template");
    template.innerHTML = html;
    const image = template.content.querySelector("img");

    expect(image?.hasAttribute("src")).toBe(false);
    expect(image?.getAttribute("data-inkflow-remote-src")).toBe(source);
  });

  it("does not treat similarly named data attributes as active image sources", () => {
    const html = '<img data-src="https://example.com/lazy.png" src="local.png">';
    expect(blockRemoteImageRequests(html)).toBe(html);
  });

  it("ignores remote image tags inside inactive raw HTML containers", async () => {
    const examples = [
      '<!-- <img src="https://example.com/comment.png"> -->',
      '<script><img src="https://example.com/script.png"></script>',
      '<template><img src="https://example.com/template.png"></template>',
    ];

    for (const markdown of examples) {
      expect(await renderMarkdown(markdown)).not.toContain("<img");
      expect(await hasRemoteImages(markdown)).toBe(false);
    }
  });

  it("does not let an inactive raw-text opener hide a later image", async () => {
    const examples = [
      [
        "<!-- <script> -->",
        '<img src="https://example.com/after-comment.png">',
      ].join("\n"),
      [
        "```html",
        "<script>",
        "```",
        '<img src="https://example.com/after-code.png">',
      ].join("\n"),
    ];

    for (const markdown of examples) {
      expect(await renderMarkdown(markdown)).toContain("<img");
      expect(await hasRemoteImages(markdown)).toBe(true);
    }
  });

  it("handles greater-than characters inside attributes without leaving an active source", () => {
    const html = blockRemoteImageRequests('<img alt=">" src="https://example.com/a.png">');
    const template = document.createElement("template");
    template.innerHTML = html;
    const image = template.content.querySelector("img");

    expect(image?.hasAttribute("src")).toBe(false);
    expect(image?.getAttribute("data-inkflow-remote-src")).toBe("https://example.com/a.png");
  });

  it("neutralizes sources that the browser normalizes to remote URLs", () => {
    const disguisedSources = [
      String.raw`https:\\example.com\a.png`,
      "https:/example.com/a.png",
      "https:example.com/a.png",
    ];
    for (const disguised of disguisedSources) {
      const html = blockRemoteImageRequests(`<img src="${disguised}">`);
      const template = document.createElement("template");
      template.innerHTML = html;
      const image = template.content.querySelector("img");

      expect(image?.hasAttribute("src")).toBe(false);
      expect(image?.getAttribute("data-inkflow-remote-src")).toBe(disguised);
    }
  });

  it("blocks only remote picture candidates before the fragment can enter the document", () => {
    const html = blockRemoteImageRequests(
      '<picture><source srcset="local.png 1x, //example.com/a.png 2x"><img src="local.png"></picture>',
    );
    const template = document.createElement("template");
    template.innerHTML = html;
    const source = template.content.querySelector("source");

    expect(source?.getAttribute("srcset")).toBe("local.png 1x");
    expect(source?.getAttribute("data-inkflow-remote-srcset"))
      .toBe("//example.com/a.png 2x");
    expect(source?.classList.contains("remote-partially-blocked")).toBe(true);
    expect(template.content.querySelector("img")?.getAttribute("src")).toBe("local.png");
  });

  it("keeps a local img fallback unblocked when its srcset is remote-only", () => {
    const html = blockRemoteImageRequests(
      '<img src="local.png" srcset="https://example.com/remote.png 2x" alt="responsive">',
    );
    const template = document.createElement("template");
    template.innerHTML = html;
    const image = template.content.querySelector("img");

    expect(image?.getAttribute("src")).toBe("local.png");
    expect(image?.hasAttribute("srcset")).toBe(false);
    expect(image?.getAttribute("data-inkflow-remote-srcset"))
      .toBe("https://example.com/remote.png 2x");
    expect(image?.classList.contains("remote-blocked")).toBe(false);
    expect(image?.classList.contains("remote-partially-blocked")).toBe(true);
  });

  it("marks an img whose only sources are remote srcset candidates", () => {
    const html = blockRemoteImageRequests(
      '<img srcset="https://example.com/remote.png 1x, //example.com/large.png 2x" alt="responsive">',
    );
    const template = document.createElement("template");
    template.innerHTML = html;
    const image = template.content.querySelector("img");

    expect(image?.hasAttribute("srcset")).toBe(false);
    expect(image?.getAttribute("data-inkflow-remote-src"))
      .toBe("https://example.com/remote.png");
    expect(image?.getAttribute("data-inkflow-remote-srcset"))
      .toBe("https://example.com/remote.png 1x, //example.com/large.png 2x");
    expect(image?.classList.contains("remote-blocked")).toBe(true);
  });

  it("only treats picture sources applicable to the current environment as usable", () => {
    const originalMatchMedia = window.matchMedia;
    window.matchMedia = vi.fn((query: string) => ({
      matches: query === "(min-width: 800px)",
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(),
    }));
    const template = document.createElement("template");
    template.innerHTML = [
      '<picture><source media="(min-width: 9999px)" srcset="wide.png"><img></picture>',
      '<picture><source media="(min-width: 800px)" type="image/webp" srcset="wide.webp"><img></picture>',
      '<picture><source type="image/jxl" srcset="wide.jxl"><img></picture>',
    ].join("");
    const images = template.content.querySelectorAll("img");

    try {
      expect(hasUsableResponsiveImageSource(images[0])).toBe(false);
      expect(hasUsableResponsiveImageSource(images[1])).toBe(true);
      expect(hasUsableResponsiveImageSource(images[2])).toBe(false);
      expect(hasRetainedResponsiveImageSource(images[0])).toBe(true);
      expect(hasRetainedResponsiveImageSource(images[1])).toBe(true);
      expect(hasRetainedResponsiveImageSource(images[2])).toBe(true);
    } finally {
      window.matchMedia = originalMatchMedia;
    }
  });

  it("neutralizes remote resources in sanitized raw HTML", async () => {
    const rendered = await renderMarkdown(
      '<picture><source srcset="//example.com/source.png"><img alt=">" src="https://example.com/fallback.png"></picture>',
    );
    const template = document.createElement("template");
    template.innerHTML = blockRemoteImageRequests(rendered);

    expect(template.content.querySelector("source")?.hasAttribute("srcset")).toBe(false);
    expect(template.content.querySelector("img")?.hasAttribute("src")).toBe(false);
  });
});
