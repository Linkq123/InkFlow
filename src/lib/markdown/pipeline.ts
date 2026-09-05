import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import remarkFrontmatter from "remark-frontmatter";
import remarkRehype from "remark-rehype";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import rehypeStringify from "rehype-stringify";
import { parseImageSrcset, serializeImageSrcset } from "./resources";

const sanitizeSchema = {
  ...defaultSchema,
  attributes: {
    ...defaultSchema.attributes,
    img: [...(defaultSchema.attributes?.img ?? []), "srcSet", "sizes"],
    source: [
      ...(defaultSchema.attributes?.source ?? []),
      "srcSet",
      "sizes",
      "type",
      "media",
    ],
  },
};

function sanitizeResponsiveImageSources() {
  return (tree: unknown) => filterResponsiveImageSources(tree);
}

function filterResponsiveImageSources(node: unknown): void {
  if (!node || typeof node !== "object") return;
  const current = node as Record<string, unknown>;
  if (
    current.type === "element"
    && (current.tagName === "img" || current.tagName === "source")
    && current.properties
    && typeof current.properties === "object"
  ) {
    const properties = current.properties as Record<string, unknown>;
    const value = properties.srcSet;
    if (typeof value === "string") {
      const retained = parseImageSrcset(value).filter(({ source }) =>
        hasAllowedImageProtocol(source)
      );
      if (retained.length > 0) {
        properties.srcSet = serializeImageSrcset(retained);
      } else {
        delete properties.srcSet;
      }
    } else if (value !== undefined) {
      delete properties.srcSet;
    }
  }

  if (Array.isArray(current.children)) {
    for (const child of current.children) filterResponsiveImageSources(child);
  }
}

function hasAllowedImageProtocol(source: string): boolean {
  const normalized = source
    .replace(/^[\u0000-\u0020]+|[\u0000-\u0020]+$/g, "")
    .replace(/[\t\n\r]/g, "")
    .replace(/\\/g, "/");
  const scheme = /^([a-z][a-z\d+.-]*):/i.exec(normalized)?.[1];
  return !scheme || /^(?:http|https)$/i.test(scheme);
}

async function createProcessor(hasRawHtml: boolean, hasMath: boolean) {
  const [rawModule, katexModule] = await Promise.all([
    hasRawHtml ? import("rehype-raw") : Promise.resolve(null),
    hasMath ? import("rehype-katex") : Promise.resolve(null),
  ]);
  const processor = unified()
    .use(remarkParse)
    .use(remarkFrontmatter, ["yaml"])
    .use(remarkGfm)
    .use(remarkMath)
    .use(remarkRehype, { allowDangerousHtml: true });
  if (rawModule) processor.use(rawModule.default);
  processor.use(rehypeSanitize, sanitizeSchema);
  processor.use(sanitizeResponsiveImageSources);
  if (katexModule) processor.use(katexModule.default);
  return processor.use(rehypeStringify);
}

type MarkdownProcessor = Awaited<ReturnType<typeof createProcessor>>;

const processors = new Map<string, Promise<MarkdownProcessor>>();

function processorFor(markdown: string): Promise<MarkdownProcessor> {
  const hasRawHtml = markdown.includes("<") && markdown.includes(">");
  const hasMath = markdown.includes("$");
  return processorForFeatures(hasRawHtml, hasMath);
}

function processorForFeatures(
  hasRawHtml: boolean,
  hasMath: boolean,
): Promise<MarkdownProcessor> {
  const key = `${Number(hasRawHtml)}${Number(hasMath)}`;
  let cached = processors.get(key);
  if (!cached) {
    cached = createProcessor(hasRawHtml, hasMath);
    processors.set(key, cached);
  }
  return cached;
}

export async function renderMarkdown(markdown: string): Promise<string> {
  const processor = await processorFor(markdown);
  const result = await processor.process(markdown);
  return String(result);
}

export async function renderMarkdownForResourceDetection(
  markdown: string,
): Promise<string> {
  const hasRawHtml = markdown.includes("<") && markdown.includes(">");
  const processor = await processorForFeatures(hasRawHtml, false);
  const result = await processor.process(markdown);
  return String(result);
}
