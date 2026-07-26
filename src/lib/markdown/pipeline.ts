import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import remarkFrontmatter from "remark-frontmatter";
import remarkRehype from "remark-rehype";
import rehypeSanitize from "rehype-sanitize";
import rehypeStringify from "rehype-stringify";

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
  processor.use(rehypeSanitize);
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
