import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./pipeline";
import { collectImageDestinations, collectImageDestinationsAsync } from "./image-destinations";
import { applyTextEdits, imagePathRewriteEdits } from "../document-state";

describe("HTML image attribute semantics", () => {
  it.each([
    { prefix: "prefix <textarea></p>\n\n", visible: true },
    { prefix: "prefix <title></p>\n\n", visible: true },
    { prefix: "prefix <style></p>\n\n", visible: true },
    { prefix: "prefix <script></p>\n\n", visible: true },
    { prefix: "prefix <textarea></p><b>\n\n", visible: true },
    { prefix: "- prefix <textarea></p>\n- ", visible: true },
    { prefix: "> prefix <textarea></p>\n>\n> ", visible: true },
    { prefix: "prefix <textarea></p> text *emphasis* ", visible: true },
    { prefix: 'prefix <textarea></p> text <img src="hidden.png">\n\n', visible: true },
    { prefix: "prefix <textarea></p> text ", visible: false },
    { prefix: "prefix <textarea></p>", visible: false },
    { prefix: "prefix <textarea></longlong>\n\n", visible: false },
    { prefix: "prefix <textarea>plain text\n\n", visible: false },
    { prefix: "<textarea></p>\n\n", visible: false },
    { prefix: "prefix <plaintext></p>\n\n", visible: false },
  ])("resets incomplete raw end-tag lookahead at HAST text boundaries: $prefix", async ({ prefix, visible }) => {
    const source = prefix + '<img src="photo.png" srcset="other.png 2x">';
    const rendered = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
    const expected = visible ? ["photo.png", "other.png"] : [];
    expect([...rendered.querySelectorAll("img")].flatMap(image => [
      image.getAttribute("src"), image.getAttribute("srcset")?.split(" ")[0],
    ])).toEqual(expected);
    const destinations = collectImageDestinations(source);
    expect(destinations.map(image => image.destination)).toEqual(expected);
    expect(await collectImageDestinationsAsync(source)).toEqual(destinations);
    expect(applyTextEdits(source, imagePathRewriteEdits(source, [
      { source: "photo.png", destination: "Copy.assets/photo.png" },
      { source: "other.png", destination: "Copy.assets/other.png" },
    ]))).toBe(visible
      ? prefix + '<img src="Copy.assets/photo.png" srcset="Copy.assets/other.png 2x">'
      : source);
  });
  it.each([
    "<div data-title=what's>hello</div>",
    '<div data-title=what"s>hello</div>',
    "<div data-note=x='unfinished>hello</div>",
    "<div hidden data-title=what's>hello</div>",
    "<div data-'title=value>hello</div>",
    '<div data-first="ok"\'flag=active>hello</div>',
  ])("keeps following images after literal quotes in HTML attributes: %s", async container => {
    const source = container + '<img src="photo.png" srcset="other.png 2x">';
    const rendered = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
    expect(rendered.querySelector("img")?.getAttribute("src")).toBe("photo.png");
    expect(rendered.querySelector("img")?.getAttribute("srcset")).toBe("other.png 2x");
    const destinations = collectImageDestinations(source);
    expect(destinations.map(image => image.destination)).toEqual(["photo.png", "other.png"]);
    expect(await collectImageDestinationsAsync(source)).toEqual(destinations);
    expect(applyTextEdits(source, imagePathRewriteEdits(source, [
      { source: "photo.png", destination: "Copy.assets/photo.png" },
      { source: "other.png", destination: "Copy.assets/other.png" },
    ]))).toBe(container + '<img src="Copy.assets/photo.png" srcset="Copy.assets/other.png 2x">');
  });
  it.each([
    { prefix: "<svg><title>", suffix: "</title></svg>", visible: true },
    { prefix: "<svg><style>", suffix: "</style></svg>", visible: true },
    { prefix: "<svg data-note=x/><title>", suffix: "</title></svg>", visible: true },
    { prefix: "<math><title>", suffix: "</title></math>", visible: true },
    { prefix: "<svg><foreignObject><title>", suffix: "</title></foreignObject></svg>", visible: false },
    { prefix: "<svg><title><title>", suffix: "</title></title></svg>", visible: false },
    { prefix: "<math><mtext><title>", suffix: "</title></mtext></math>", visible: false },
    { prefix: '<math><annotation-xml encoding="text&#47;html"><title>', suffix: "</title></annotation-xml></math>", visible: false },
    { prefix: '<math><annotation-xml encoding="application/xml"><title>', suffix: "</title></annotation-xml></math>", visible: true },
    { prefix: "<math><mtext><mglyph><title>", suffix: "</title></mglyph></mtext></math>", visible: true },
    { prefix: "<svg/><title>", suffix: "</title>", visible: false },
    { prefix: "<svg><g><div><title>", suffix: "</title></div></g></svg>", visible: false },
    { prefix: "<svg></p><title>", suffix: "</title>", visible: false },
    { prefix: "<svg><![CDATA[<title>", suffix: "</title>]]></svg>", visible: false },
    { prefix: '---\ntitle: "<svg>"\n---\n\n<title>', suffix: "</title>", visible: false },
    { prefix: "prefix <svg><title>", suffix: "</title></svg>", visible: true },
    { prefix: "<svg>\n\nordinary paragraph\n\n<title>", suffix: "</title></svg>", visible: false },
    { prefix: '<svg><title><img src="first.png"></title></svg><title>', suffix: "</title>", visible: false, first: true },
  ])("uses the element namespace for raw text: $prefix", async ({ prefix, suffix, visible, first }) => {
    const source = prefix + '<img src="a.png">' + suffix + '\n\n<img src="b.png">';
    const expected = [...(first ? ["first.png"] : []), ...(visible ? ["a.png"] : []), "b.png"];
    const rendered = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
    expect([...rendered.querySelectorAll("img")].map(image => image.getAttribute("src"))).toEqual(expected);
    const destinations = collectImageDestinations(source);
    expect(destinations.map(image => image.destination)).toEqual(expected);
    expect(await collectImageDestinationsAsync(source)).toEqual(destinations);
    const rewritten = applyTextEdits(source, imagePathRewriteEdits(source, [
      { source: "a.png", destination: "Copy.assets/a.png" },
      { source: "b.png", destination: "Copy.assets/b.png" },
      { source: "first.png", destination: "Copy.assets/first.png" },
    ]));
    let expectedSource = source.replace("b.png", "Copy.assets/b.png");
    if (first) expectedSource = expectedSource.replace("first.png", "Copy.assets/first.png");
    if (visible) expectedSource = expectedSource.replace("a.png", "Copy.assets/a.png");
    expect(rewritten).toBe(expectedSource);
  });
  it.each([
    { prefix: "prefix <textarea>text *emphasis*", visible: true },
    { prefix: "prefix <textarea>text **strong**", visible: true },
    { prefix: "prefix <textarea>text `code`", visible: true },
    { prefix: "prefix <textarea>text\n\nordinary paragraph", visible: true, separator: "\n\n" },
    { prefix: "*prefix <textarea>text*", visible: false },
    { prefix: "prefix <textarea>*![literal](hidden.png)*", visible: false },
    { prefix: "prefix <plaintext>text *emphasis*", visible: false },
  ])("resumes HTML images only after the raw tokenizer resets: $prefix", async ({ prefix, visible, separator = " " }) => {
    const source = prefix + separator + '<img src="a.png">';
    const rendered = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
    const expected = visible ? ["a.png"] : [];
    expect([...rendered.querySelectorAll("img")].map(image => image.getAttribute("src"))).toEqual(expected);
    const destinations = collectImageDestinations(source);
    expect(destinations.map(image => image.destination)).toEqual(expected);
    expect(await collectImageDestinationsAsync(source)).toEqual(destinations);
    expect(applyTextEdits(source, imagePathRewriteEdits(source, [
      { source: "a.png", destination: "Copy.assets/a.png" },
      { source: "hidden.png", destination: "Copy.assets/hidden.png" },
    ]))).toBe(visible ? prefix + separator + '<img src="Copy.assets/a.png">' : source);
  });
  it.each(["title", "textarea", "plaintext"].flatMap(tag => [
    { context: "YAML", prefix: '---\ntitle: "<' + tag + '>"\n---' },
    { context: "inline math", prefix: "$<" + tag + ">$" },
    { context: "display math", prefix: "$$\n<" + tag + ">\n$$" },
  ]))("ignores raw-text tag literals in $context: $prefix", async ({ prefix }) => {
    const source = prefix + '\n\n<img src="a.png">\n\n![real](b.png)';
    const rendered = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
    expect([...rendered.querySelectorAll("img")].map(image => image.getAttribute("src"))).toEqual(["a.png", "b.png"]);
    const destinations = collectImageDestinations(source);
    expect(destinations.map(image => image.destination)).toEqual(["a.png", "b.png"]);
    expect(await collectImageDestinationsAsync(source)).toEqual(destinations);
    expect(applyTextEdits(source, imagePathRewriteEdits(source, [
      { source: "a.png", destination: "Copy.assets/a.png" },
      { source: "b.png", destination: "Copy.assets/b.png" },
    ]))).toBe(prefix + '\n\n<img src="Copy.assets/a.png">\n\n![real](Copy.assets/b.png)');
  });
  it.each([
    { source: "prefix <textarea>text\n\n![real](a.png)\n\n</textarea>", images: ["a.png"] },
    { source: "prefix <script>text\n\n![real](a.png)\n\n</script>", images: ["a.png"] },
    { source: "# prefix <textarea>text\n\n![real](a.png)\n\n</textarea>", images: ["a.png"] },
    { source: "- prefix <textarea>text\n- ![real](a.png)\n\n</textarea>", images: ["a.png"] },
    { source: "prefix <textarea>text\n![literal](hidden.png)\n\n![real](a.png)\n\n</textarea>", images: ["a.png"] },
    { source: "prefix <textarea>text *emphasis* ![real](a.png)", images: ["a.png"] },
    { source: "<textarea>text\n\n![literal](hidden.png)\n\n</textarea>\n\n![real](a.png)", images: ["a.png"] },
    { source: "prefix\n<textarea>text\n\n![literal](hidden.png)\n\n</textarea>\n\n![real](a.png)", images: ["a.png"] },
    { source: "- <textarea>text\n\n  ![literal](hidden.png)\n\n  </textarea>\n\n![real](a.png)", images: ["a.png"] },
    { source: "> <textarea>text\n>\n> ![literal](hidden.png)\n>\n> </textarea>\n\n![real](a.png)", images: ["a.png"] },
    { source: "prefix <textarea>text\n\n<img src=\"hidden.png\">\n\n</textarea>", images: [] },
  ])("keeps raw-text image discovery aligned with rendered containers: $source", async ({ source, images }) => {
    const rendered = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
    expect([...rendered.querySelectorAll("img")].map(image => image.getAttribute("src"))).toEqual(images);
    expect(collectImageDestinations(source).map(image => image.destination)).toEqual(images);
    const rewritten = applyTextEdits(source, imagePathRewriteEdits(source, [
      { source: "a.png", destination: "Copy.assets/a.png" },
      { source: "hidden.png", destination: "Copy.assets/hidden.png" },
    ]));
    expect(rewritten).toBe(images.length ? source.replace("a.png", "Copy.assets/a.png") : source);
  });
  it.each([
    { label: "<textarea>", reference: false }, { label: "<textarea>", reference: true },
    { label: "<script>", reference: false }, { label: "<script>", reference: true },
    { label: '<img src="literal.png">', reference: false },
    { label: '<img src="literal.png">', reference: true },
  ])("treats HTML in image alternative text as literal: $label, reference=$reference", async ({ label, reference }) => {
    const image = reference ? "![" + label + "][picture]\n\n[picture]: a.png" : "![" + label + "](a.png)";
    const source = image + "\n\n![real](b.png)";
    const rendered = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
    expect([...rendered.querySelectorAll("img")].map(node => node.getAttribute("src"))).toEqual(["a.png", "b.png"]);
    expect(rendered.querySelector("img")?.getAttribute("alt")).toBe(label);
    expect(collectImageDestinations(source).map(node => node.destination)).toEqual(["a.png", "b.png"]);
    expect(applyTextEdits(source, imagePathRewriteEdits(source, [
      { source: "a.png", destination: "Copy.assets/a.png" },
      { source: "b.png", destination: "Copy.assets/b.png" },
      { source: "literal.png", destination: "Copy.assets/literal.png" },
    ]))).toBe(image.replace("a.png", "Copy.assets/a.png") + "\n\n![real](Copy.assets/b.png)");
  });
  it.each(["    </textarea>", "\t</textarea>"])("finds real images after an end tag outside HTML nodes: %s", async closing => {
    const literal = "<textarea>abc\n\n" + closing;
    const source = literal + "\n\n![real](real.png)\n\n<img src=\"other.png\">";
    const html = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
    expect([...html.querySelectorAll("img")].map(image => image.getAttribute("src"))).toEqual(["real.png", "other.png"]);
    expect(collectImageDestinations(source).map(image => image.destination)).toEqual(["real.png", "other.png"]);
    expect(applyTextEdits(source, imagePathRewriteEdits(source, [
      { source: "real.png", destination: "Copy.assets/real.png" },
      { source: "other.png", destination: "Copy.assets/other.png" },
    ]))).toBe(literal + "\n\n![real](Copy.assets/real.png)\n\n<img src=\"Copy.assets/other.png\">");
  });
  it.each([
    '<div title="<img src=\'literal.png\'>">literal</div>',
    '<script>const x = "<img src=\'literal.png\'>";</script>',
    '<textarea><img src="literal.png"></textarea>',
    '<style>p::before {content: "<img src=\'literal.png\'>";}</style>',
    'prefix <textarea>literal <img src="literal.png"></textarea> suffix',
    '<TITLE><img src="literal.png"></TiTlE>',
  ])("leaves image-like text opaque but migrates a real following image: %s", async literal => {
    const source = literal + '\n\n<img src="real.png">';
    const html = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
    expect([...html.querySelectorAll("img")].map(image => image.getAttribute("src"))).toEqual(["real.png"]);
    expect(collectImageDestinations(source).map(image => image.destination)).toEqual(["real.png"]);
    expect(applyTextEdits(source, imagePathRewriteEdits(source, [
      { source: "literal.png", destination: "Copy.assets/literal.png" },
      { source: "real.png", destination: "Copy.assets/real.png" },
    ]))).toBe(literal + '\n\n<img src="Copy.assets/real.png">');
  });
  it.each([
    "a&amp-b.png", "a&#38-b.png", "a&#x26-b.png", "a&copy-b.png",
    "a&amp=1.png", "a&notit.png", "a&#128;.png", "a&#0;.png", "a&NotEqualTilde;.png",
  ])("agrees with the rendered HTML and preserves rewrite ranges: %s", async spelling => {
    for (const attribute of ["src", "srcset"]) {
      const source = '<img ' + attribute + '="' + spelling + (attribute === "srcset" ? " 1x" : "") + '">';
      const html = new DOMParser().parseFromString(await renderMarkdown(source), "text/html");
      const rendered = html.querySelector("img")!.getAttribute(attribute)!;
      const destination = attribute === "srcset" ? rendered.slice(0, -3) : rendered;
      expect(collectImageDestinations(source).map(item => item.destination)).toEqual([destination]);
      const result = applyTextEdits(source, imagePathRewriteEdits(source, [{
        source: destination, destination: "Copy.assets/image.png",
      }]));
      expect(result).toBe('<img ' + attribute + '="Copy.assets/image.png' + (attribute === "srcset" ? " 1x" : "") + '">');
    }
  });
});
