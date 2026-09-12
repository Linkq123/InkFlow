import { describe, expect, it } from "vitest";
import fixtures from "../../../tests/fixtures/mermaid-image-rewrites.json";
import { JSON_SCHEMA, load } from "js-yaml";
import { applyTextEdits, imagePathRewriteEdits, imageRewriteEditsBetween, mergeImageRewrites } from "../document-state";
import { collectImageDestinations } from "./image-destinations";
import { collectMermaidImageReferences, extractMermaidMetadataBlocks, MAX_MERMAID_SOURCE } from "./mermaid-metadata";
import { hasRemoteMermaidImageReference, resolveLocalMermaidImageReferences } from "./resources";

describe("Mermaid image migration", () => {
  it.each(fixtures)("preserves source ranges and YAML semantics: $name", fixture => {
    const resources = collectImageDestinations(fixture.content).map(image => image.destination);
    for (const asset of fixture.assets) expect(resources).toContain(asset);
    const rewrites = fixture.assets.map(source => ({ source, destination: `Copy.assets/${source.split("/").at(-1)}` }));
    const rewritten = applyTextEdits(fixture.content, imagePathRewriteEdits(fixture.content, rewrites));
    expect(rewritten).toBe(fixture.rewritten);
    expect(applyTextEdits(fixture.content, imageRewriteEditsBetween(fixture.content, rewritten))).toBe(rewritten);
    expect(mergeImageRewrites(fixture.content + "\nLatest edit", fixture.content, rewritten)).toBe(rewritten + "\nLatest edit");
    for (const { destination } of rewrites) expect(collectImageDestinations(rewritten).map(image => image.destination)).toContain(destination);
  });

  it("preserves scalar aliases inside shared and recursive metadata without expanding them", async () => {
    const source = 'flowchart LR\nA@{\nimg: &pic images/a.png\nextra: &loop [*pic, *loop]\ncopy: *loop\nlabel: *pic\n}';
    const rewritten = await resolveLocalMermaidImageReferences(source, async () => "data:image/png;base64,AA==");
    const parsed = load(extractMermaidMetadataBlocks(rewritten)[0].content, { schema: JSON_SCHEMA }) as Record<string, unknown>;
    const extra = parsed.extra as unknown[];
    expect(parsed.img).toBe("data:image/png;base64,AA==");
    expect(parsed.label).toBe("images/a.png");
    expect(extra[0]).toBe("images/a.png");
    expect(extra[1]).toBe(extra);
    expect(parsed.copy).toBe(extra);
    expect(rewritten.length).toBeLessThan(source.length + 100);
  });

  it("encodes generated URLs once and preserves quoted metadata delimiters", () => {
    const content = '```mermaid\nflowchart LR\nA@{img: "x.png"}\n```';
    const rewritten = applyTextEdits(content, imagePathRewriteEdits(content, [{ source: "x.png", destination: "Copy name%26more.assets/x.png" }]));
    expect(rewritten).toContain('img: "Copy name%26more.assets/x.png"');
  });
});

describe("bounded Mermaid metadata scanning", () => {
  it.each([4000, 8000, 16000])("does not rescan malformed suffixes (%i markers)", count => {
    for (const source of ["flowchart LR\nA" + "@{".repeat(count), 'A@{img:"' + "@{".repeat(count), "A@{" + "[".repeat(count)]) {
      let reads = 0;
      const instrumented = new Proxy(new String(source), { get(_target, key) {
        if (typeof key === "string" && /^\d+$/.test(key)) reads++;
        const value = Reflect.get(Object(source), key);
        return typeof value === "function" ? value.bind(source) : value;
      } }) as unknown as string;
      try { extractMermaidMetadataBlocks(instrumented); } catch (error) { expect(String(error)).toContain("nesting"); }
      expect(reads).toBeLessThanOrEqual(source.length * 2);
    }
  });

  it("bounds total input and indentation-based YAML nesting before hydration", async () => {
    const oversized = "A@{" + "x".repeat(MAX_MERMAID_SOURCE);
    expect(() => collectMermaidImageReferences(oversized)).toThrow("limit");
    await expect(hasRemoteMermaidImageReference(oversized)).resolves.toBe(true);
    const nested = "A@{\nimg: x.png\n" + Array.from({ length: 80 }, (_, depth) => " ".repeat(depth) + "nested:").join("\n") + "\n}";
    await expect(resolveLocalMermaidImageReferences(nested, async () => "unused")).rejects.toThrow("safely resolved");
  });
});
