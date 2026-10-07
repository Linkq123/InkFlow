import { commonmarkLanguage } from "@codemirror/lang-markdown";
import { decodeHTMLAttribute, decodeHTMLStrict, DecodingMode, EntityDecoder, htmlDecodeTree } from "entities/decode";
import { cooperativeWork, type WorkCheckpoint } from "../async";
import { collectMermaidImageReferences, encodeMermaidImageReference, type MermaidAliasEdit } from "./mermaid-metadata";
import { createMarkdownParser } from "./parser";
import remarkRehype from "remark-rehype";
import { HtmlNamespaceContext } from "./html-context";

export interface ImageDestination {
  raw: string;
  /** Markup escapes/entities decoded; URL percent escapes remain untouched. */
  destination: string;
  from: number;
  to: number;
  syntax: "markdown" | "html" | "mermaid";
  trailingNewline?: string;
  preservedAlias?: MermaidAliasEdit;
  quote?: string | null;
  attribute?: "src" | "srcset";
  imageFrom?: number;
  imageTo?: number;
}

const asciiWhitespace = (value: string) => /[\t\n\v\f\r ]/.test(value);

// Keep these source ranges aligned with asset.rs. DOM parsing loses original
// quoting/offsets, and a quoted-src regex misses responsive and unquoted images.
interface HtmlScanState {
  rawTo: number;
  opaque: Array<{ from: number; to: number }>;
  html: HtmlNamespaceContext;
  boundaries: (offset: number, rawText?: { tag: string; from: number; to: number }) => { markdownTo: number; htmlTo: number } | undefined;
}

function rawTextBoundaries(markdown: string, html: HtmlNamespaceContext): HtmlScanState["boundaries"] {
  // Lezer does not classify every CommonMark raw HTML block (notably textarea).
  // Use the renderer's generated elements, including omitted tight-list
  // paragraphs and void elements, to distinguish the two raw-text boundaries.
  type Boundary = { index: number; to: number };
  type Token = { kind: "raw" | "text" | "start" | "end"; from: number; to: number; tag?: string };
  let context: {
    htmlRanges: Array<{ from: number; to: number; index: number }>;
    markdownEnds: Boundary[];
    htmlResets: Boundary[];
    generated: Array<{ index: number; tag: string; closing: boolean }>;
    tokens: Token[];
  } | undefined;
  let generatedIndex = 0;
  const firstAfter = (points: Boundary[], index: number): number => {
    let low = 0, high = points.length;
    while (low < high) {
      const mid = (low + high) >>> 1;
      if (points[mid].index <= index) low = mid + 1;
      else high = mid;
    }
    return points[low]?.to ?? markdown.length;
  };
  return (offset, rawText) => {
    if (!context) {
      context = { htmlRanges: [], markdownEnds: [], htmlResets: [], generated: [], tokens: [] };
      type Node = { type: string; tagName?: string; position?: { start: { offset?: number }; end: { offset?: number } }; children?: Node[] };
      const voidElements = new Set([
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
      ]);
      let index = 0;
      let sourceEnd = 0;
      let lastStart: { tag: string; index: number } | undefined;
      const visit = (node: Node, parentEnd: number): void => {
        const from = node.position?.start.offset;
        const to = node.position?.end.offset ?? parentEnd;
        const tag = node.type === "element" ? node.tagName : undefined;
        if (tag) {
          lastStart = { tag, index: index++ };
          context!.generated.push({ ...lastStart, closing: false });
          context!.tokens.push({ kind: "start", from: from ?? sourceEnd, to, tag });
        }
        if (node.type === "raw" && from !== undefined) {
          context!.htmlRanges.push({ from, to, index: index++ });
          context!.tokens.push({ kind: "raw", from, to });
          sourceEnd = to;
        }
        if (node.type === "text") {
          index++;
          // remark-rehype inserts unpositioned newlines between block nodes.
          context!.tokens.push({ kind: "text", from: from ?? sourceEnd, to });
          if (node.position) sourceEnd = to;
        }
        node.children?.forEach(child => visit(child, to));
        if (tag && !voidElements.has(tag)) {
          context!.generated.push({ index, tag, closing: true });
          context!.markdownEnds.push({ index: index++, to });
          context!.tokens.push({ kind: "end", from: to, to, tag });
          sourceEnd = to;
          // rehype-raw resets its tokenizer only when the generated closing
          // tag matches the last generated start after the raw HTML opener.
          // A void start (e.g. img) still replaces that last-start marker.
          if (lastStart?.tag === tag) context!.htmlResets.push({ index: lastStart.index, to });
        }
      };
      const processor = createMarkdownParser().use(remarkRehype, { allowDangerousHtml: true });
      visit(processor.runSync(processor.parse(markdown)), markdown.length);
      context.htmlRanges.sort((left, right) => left.from - right.from);
    }
    // Tags inside YAML or math can look like HTML to Lezer, but they never
    // reach the renderer's HTML tokenizer and must not open raw-text scopes.
    let low = 0, high = context.htmlRanges.length;
    while (low < high) {
      const mid = (low + high) >>> 1;
      if (context.htmlRanges[mid].to <= offset) low = mid + 1;
      else high = mid;
    }
    const range = context.htmlRanges[low];
    if (!range || offset < range.from) return undefined;
    while (generatedIndex < context.generated.length && context.generated[generatedIndex].index < range.index) {
      const event = context.generated[generatedIndex++];
      html.generated(event.tag, event.closing);
    }
    const markdownTo = firstAfter(context.markdownEnds, range.index);
    let htmlTo = firstAfter(context.htmlResets, range.index);
    if (rawText && rawText.tag !== "plaintext") {
      htmlTo = Math.min(htmlTo, rawText.to);
      let lastStart = rawText.tag;
      let pending = false;
      for (let index = range.index; index < context.tokens.length; index++) {
        const token = context.tokens[index];
        if (token.from >= htmlTo) break;
        if (token.kind === "start") lastStart = token.tag!;
        else if (token.kind === "raw") {
          pending = incompleteRawEndTag(markdown.slice(Math.max(token.from, rawText.from), Math.min(token.to, htmlTo)), lastStart, pending);
        } else if (token.kind === "text" && pending) {
          // rehype-raw resets a suspended tokenizer at a HAST text node.
          // Its tree builder can still be inside the raw element until the
          // next generated end, so both boundaries must have been crossed.
          htmlTo = Math.min(htmlTo, Math.max(token.from, markdownTo));
          break;
        }
      }
    }
    return { markdownTo, htmlTo };
  };
}

function incompleteRawEndTag(source: string, lastStart: string, pending: boolean): boolean {
  // parse5 checks the whole expected name before testing for a mismatch.
  // A short nonmatching end tag can therefore suspend at a raw-node boundary.
  // A following HAST text node resets this intermediate state (but not stable
  // RCDATA/RAWTEXT). Keep this lookahead aligned with asset.rs.
  if (pending && source.length < lastStart.length) return true;
  if (source.endsWith("<") || source.endsWith("</")) return true;
  const start = source.lastIndexOf("</") + 2;
  return start >= 2 && /^[a-z]/i.test(source.slice(start)) && source.length - start < lastStart.length;
}

function htmlImageDestinations(source: string, offset: number, state: HtmlScanState, markdown: string): ImageDestination[] {
  const result: ImageDestination[] = [];
  let cursor = 0;
  while (cursor < source.length) {
    cursor = Math.max(cursor, state.rawTo - offset);
    if (cursor >= source.length) break;
    const start = source.indexOf("<", cursor);
    if (start < 0) break;
    if (source.startsWith("<!--", start)) {
      const end = source.indexOf("-->", start + 4);
      cursor = end < 0 ? source.length : end + 3;
      continue;
    }
    if (state.html.foreign && source.startsWith("<![CDATA[", start)) {
      const end = source.indexOf("]]>", start + 9);
      cursor = end < 0 ? source.length : end + 3;
      continue;
    }
    const tag = /^(\/?)([a-z][^\t\n\f\r />]*)(?=[\t\n\f\r />])/i.exec(source.slice(start + 1));
    cursor = start + 1;
    if (!tag) continue;
    cursor += tag[0].length;
    const end = htmlTagEnd(source, cursor);
    if (end === source.length) break;
    const tagName = tag[2].toLowerCase();
    if ((state.html.active || tagName === "svg" || tagName === "math")
      && state.boundaries(offset + start) === undefined) {
      cursor = end + 1;
      continue;
    }
    if (tag[1]) {
      state.html.end(tagName);
      cursor = end + 1;
      continue;
    }
    const attributes = (tagName === "annotation-xml" || tagName === "font" || source[end - 1] === "/")
      ? [...htmlTagAttributes(source, cursor, end)] : [];
    const encoding = attributes.find(attribute => attribute.name === "encoding");
    const selfClosing = source[end - 1] === "/"
      && !attributes.some(attribute => attribute.hasValue && attribute.quote === null && attribute.to === end);
    const isHtml = state.html.start(tagName, selfClosing,
      encoding ? decodeHTMLAttribute(source.slice(encoding.from, encoding.to)) : undefined,
      attributes.some(attribute => ["color", "size", "face"].includes(attribute.name)));
    if (!["img", "source"].includes(tagName)) {
      cursor = end + 1;
      if (isHtml && ["script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes", "plaintext"].includes(tagName)) {
        // Markdown nodes may classify a raw-text closing tag as indented code.
        // Locate its literal boundary in the original source, including gaps
        // between HTML nodes, before filtering any later image destinations.
        const closing = new RegExp("</" + tagName + "(?=[\\t\\n\\f\\r />])", "ig");
        closing.lastIndex = offset + cursor;
        const literalEnd = (tagName === "plaintext" ? null : closing.exec(markdown))?.index ?? markdown.length;
        const boundaries = state.boundaries(offset + start, { tag: tagName, from: offset + cursor, to: literalEnd });
        if (boundaries === undefined) continue;
        state.html.enterRaw(tagName);
        state.rawTo = literalEnd;
        // Markdown emits its own closing elements at container boundaries.
        // Those end the raw-text element even without a literal closing tag.
        // Literal HTML resumes only after the tokenizer also leaves raw text.
        if (tagName !== "plaintext") state.rawTo = Math.min(state.rawTo, boundaries.htmlTo);
        const markdownTo = tagName === "plaintext" ? markdown.length : boundaries.markdownTo;
        state.opaque.push({ from: offset + cursor, to: Math.min(state.rawTo, markdownTo) });
      }
      continue;
    }
    for (const { name, from: valueStart, to: valueEnd, quote, hasValue } of htmlTagAttributes(source, cursor, end)) {
      if (!hasValue) continue;
      if (name !== "srcset" && !(tagName === "img" && name === "src")) continue;
      const ranges = name === "srcset"
        ? htmlSrcsetDestinations(source.slice(valueStart, valueEnd))
          .map(({ from, to, destination }) => ({ from: from + valueStart, to: to + valueStart, destination }))
        : [{ from: valueStart, to: valueEnd, destination: decodeHTMLAttribute(source.slice(valueStart, valueEnd)) }];
      for (const { from, to, destination } of ranges) {
        const raw = source.slice(from, to);
        result.push({
          raw, destination, from: offset + from, to: offset + to,
          syntax: "html", quote, attribute: name === "srcset" ? "srcset" : "src",
        });
      }
    }
    cursor = end + 1;
  }
  return result;
}

// Keep attribute-state transitions aligned with html_tag_end in asset.rs.
// Quotes open a quoted value only immediately after '=' and optional space.
// In unquoted values or attribute names they are literal parse-error characters.
function htmlTagEnd(source: string, cursor: number): number {
  const isSpace = (character: string) => /[\t\n\f\r ]/.test(character);
  while (cursor < source.length) {
    while (isSpace(source[cursor])) cursor++;
    if (source[cursor] === ">") return cursor;
    if (source[cursor] === "/") { cursor++; continue; }
    // A leading '=' is part of an invalid attribute name, not a value opener.
    if (source[cursor] === "=") cursor++;
    while (cursor < source.length && !isSpace(source[cursor]) && !"/=>".includes(source[cursor])) cursor++;
    while (isSpace(source[cursor])) cursor++;
    if (source[cursor] !== "=") continue;
    cursor++;
    while (isSpace(source[cursor])) cursor++;
    const quote = source[cursor] === "'" || source[cursor] === '"' ? source[cursor++] : null;
    if (quote) {
      while (cursor < source.length && source[cursor] !== quote) cursor++;
      if (cursor === source.length) return cursor;
      cursor++;
    } else {
      while (cursor < source.length && !isSpace(source[cursor]) && source[cursor] !== ">") cursor++;
    }
  }
  return source.length;
}

function* htmlTagAttributes(source: string, cursor: number, end: number) {
  while (cursor < end) {
    while (cursor < end && asciiWhitespace(source[cursor])) cursor++;
    if (cursor >= end || source[cursor] === "/") break;
    const nameStart = cursor;
    while (cursor < end && !asciiWhitespace(source[cursor]) && !"/=>".includes(source[cursor])) cursor++;
    if (cursor === nameStart) { cursor++; continue; }
    const name = source.slice(nameStart, cursor).toLowerCase();
    while (cursor < end && asciiWhitespace(source[cursor])) cursor++;
    if (source[cursor] !== "=") {
      yield { name, from: cursor, to: cursor, quote: null, hasValue: false };
      continue;
    }
    cursor++;
    while (cursor < end && asciiWhitespace(source[cursor])) cursor++;
    const quote = source[cursor] === "'" || source[cursor] === '"' ? source[cursor++] : null;
    const from = cursor;
    while (cursor < end && (quote ? source[cursor] !== quote : !asciiWhitespace(source[cursor]))) cursor++;
    const to = cursor;
    if (quote && cursor < end) cursor++;
    yield { name, from, to, quote, hasValue: true };
  }
}

// Parse candidates after HTML decoding, but replace only their original source
// spans. Descriptors and entity-encoded separators retain their exact spelling.
// Keep the entity scan and offset mapping aligned with asset.rs.
function htmlSrcsetDestinations(source: string): Array<{ from: number; to: number; destination: string }> {
  if (!source.includes("&")) {
    return srcsetRanges(source).map(range => ({ ...range, destination: source.slice(range.from, range.to) }));
  }
  let value = "";
  const offsets = [0];
  let cursor = 0;
  const appendLiteral = (end: number) => {
    value += source.slice(cursor, end);
    while (cursor < end) offsets.push(++cursor);
  };
  let decoded = "";
  const decoder = new EntityDecoder(htmlDecodeTree, codepoint => { decoded += String.fromCodePoint(codepoint); });
  for (let start = source.indexOf("&"); start !== -1; start = source.indexOf("&", cursor)) {
    appendLiteral(start);
    decoded = "";
    decoder.startEntity(DecodingMode.Attribute);
    let consumed = decoder.write(source, start + 1);
    if (consumed < 0) consumed = decoder.end();
    if (!consumed) appendLiteral(cursor + 1);
    else {
      cursor += consumed;
      value += decoded;
      for (let index = 0; index < decoded.length; index++) offsets.push(cursor);
    }
  }
  appendLiteral(source.length);
  return srcsetRanges(value).map(({ from, to }) => ({
    from: offsets[from], to: offsets[to], destination: value.slice(from, to),
  }));
}

function srcsetRanges(value: string): Array<{ from: number; to: number }> {
  const ranges: Array<{ from: number; to: number }> = [];
  let cursor = 0;
  while (cursor < value.length) {
    while (cursor < value.length && (asciiWhitespace(value[cursor]) || value[cursor] === ",")) cursor++;
    const from = cursor;
    while (cursor < value.length && !asciiWhitespace(value[cursor])) cursor++;
    let to = cursor;
    while (to > from && value[to - 1] === ",") to--;
    if (to > from) ranges.push({ from, to });
    if (to < cursor) continue;
    let parentheses = 0;
    while (cursor < value.length) {
      const character = value[cursor++];
      if (character === "(") parentheses++;
      else if (character === ")") parentheses = Math.max(0, parentheses - 1);
      else if (character === "," && parentheses === 0) break;
    }
  }
  return ranges;
}

export function collectImageDestinations(markdown: string): ImageDestination[] {
  return imageDestinationsFromTree(markdown, commonmarkLanguage.parser.parse(markdown));
}

export async function collectImageDestinationsAsync(
  markdown: string,
  checkpoint: WorkCheckpoint = cooperativeWork(),
): Promise<ImageDestination[]> {
  const parse = commonmarkLanguage.parser.startParse(markdown);
  for (;;) {
    const pause = checkpoint();
    if (pause) await pause;
    const tree = parse.advance();
    if (tree) return imageDestinationsFromTree(markdown, tree);
  }
}

function imageDestinationsFromTree(
  markdown: string,
  tree: ReturnType<typeof commonmarkLanguage.parser.parse>,
): ImageDestination[] {
  const destinations: ImageDestination[] = [];
  const referenceLabels = new Set<string>();
  const referenceDefinitions = new Map<string, ImageDestination>();
  const htmlRanges: Array<{ from: number; to: number }> = [];

  tree.iterate({
    enter(node) {
      if (node.name === "FencedCode") {
        const info = node.node.getChild("CodeInfo");
        if (info && markdown.slice(info.from, info.to).trim().split(/\s/)[0] === "mermaid") {
          const parts = node.node.getChildren("CodeText");
          if (parts.length) {
            let code = "";
            const segments = parts.map(part => {
              const from = code.length;
              code += markdown.slice(part.from, part.to);
              return { from, to: code.length, source: part.from };
            });
            const sourceOffset = (offset: number): number => {
              let low = 0, high = segments.length - 1;
              while (low < high) {
                const mid = (low + high) >>> 1;
                if (segments[mid].to <= offset) low = mid + 1;
                else high = mid;
              }
              return segments[low].source + offset - segments[low].from;
            };
            try {
              for (const reference of collectMermaidImageReferences(code)) {
                const from = sourceOffset(reference.from);
                const to = sourceOffset(reference.to - 1) + 1;
                destinations.push({ raw: markdown.slice(from, to), destination: reference.source, from, to,
                  syntax: "mermaid", trailingNewline: reference.trailingNewline,
                  ...(reference.preservedAlias ? { preservedAlias: {
                    ...reference.preservedAlias,
                    from: sourceOffset(reference.preservedAlias.from),
                    to: sourceOffset(reference.preservedAlias.to - 1) + 1,
                  } } : {}),
                });
              }
            } catch { /* Malformed diagrams stay as inert source, matching rendering. */ }
          }
        }
        return false;
      }
      if (node.name === "Image") {
        // Children describe alternative text, not independently rendered HTML
        // or images. Collect only this image's destination.
        const url = node.node.getChild("URL");
        if (url) {
          destinations.push({ ...markdownDestination(markdown, url.from, url.to), imageFrom: node.from, imageTo: node.to });
          return false;
        }

        const marks = node.node.getChildren("LinkMark");
        if (marks.length < 2) return false;
        const alt = markdown.slice(marks[0].to, marks[1].from);
        const labelNode = node.node.getChild("LinkLabel");
        const explicitLabel = labelNode
          ? stripLabelBrackets(markdown.slice(labelNode.from, labelNode.to))
          : "";
        referenceLabels.add(normalizeReferenceLabel(explicitLabel || alt));
        return false;
      }

      if (node.name === "LinkReference") {
        const label = node.node.getChild("LinkLabel");
        const url = node.node.getChild("URL");
        if (!label || !url) return;
        const normalized = normalizeReferenceLabel(
          stripLabelBrackets(markdown.slice(label.from, label.to)),
        );
        if (!referenceDefinitions.has(normalized)) {
          referenceDefinitions.set(
            normalized,
            markdownDestination(markdown, url.from, url.to),
          );
        }
        return;
      }

      if (node.name === "HTMLTag" || node.name === "HTMLBlock") {
        htmlRanges.push({ from: node.from, to: node.to });
      }
    },
  });

  for (const label of referenceLabels) {
    const definition = referenceDefinitions.get(label);
    if (definition) destinations.push(definition);
  }

  const html = new HtmlNamespaceContext();
  const htmlState: HtmlScanState = { rawTo: 0, opaque: [], html, boundaries: rawTextBoundaries(markdown, html) };
  for (const range of htmlRanges) {
    const source = markdown.slice(range.from, range.to);
    destinations.push(...htmlImageDestinations(source, range.from, htmlState, markdown));
  }

  destinations.sort((left, right) => left.from - right.from || left.to - right.to);
  let opaqueIndex = 0;
  return destinations.filter((destination, index) => {
    while (opaqueIndex < htmlState.opaque.length && htmlState.opaque[opaqueIndex].to <= destination.from) opaqueIndex++;
    const opaque = htmlState.opaque[opaqueIndex];
    if (opaque && opaque.from <= destination.from && destination.to <= opaque.to) return false;
    const previous = destinations[index - 1];
    return !previous
      || previous.from !== destination.from
      || previous.to !== destination.to;
  });
}

function markdownDestination(
  markdown: string,
  from: number,
  to: number,
): ImageDestination {
  const raw = markdown.slice(from, to);
  const angleWrapped = raw.startsWith("<") && raw.endsWith(">");
  return {
    raw,
    destination: decodeMarkdownDestination(angleWrapped ? raw.slice(1, -1) : raw),
    from,
    to,
    syntax: "markdown",
  };
}

export function decodeMarkdownDestination(value: string): string {
  // Decode escapes and character references in one pass: `\&amp;` denotes the
  // literal string `&amp;`, not an ampersand character reference.
  return value.replace(
    /\\([!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~])|&(?:#[xX][0-9a-fA-F]{1,6}|#[0-9]{1,7}|[A-Za-z][A-Za-z0-9]{1,31});/g,
    (match, escaped: string | undefined) => escaped ?? decodeHTMLStrict(match),
  );
}

/** Encode a decoded, backend-generated asset path in the current source syntax. */
export function encodeImageDestinationPath(path: string, context: ImageDestination): string {
  if (context.syntax === "mermaid") {
    return encodeMermaidImageReference(path.replace(/%/g, "%25").replace(/&/g, "%26"), context.trailingNewline);
  }
  const encoded = Array.from(path, (character) => {
    const syntaxDelimiter = context.syntax === "markdown"
      ? "\\<>".includes(character)
      : context.quote
        ? character === context.quote
        : asciiWhitespace(character) || "\"'`<>=".includes(character);
    const candidateDelimiter = context.attribute === "srcset"
      && (asciiWhitespace(character) || character === ",");
    return character === "%" || character === "&" || syntaxDelimiter || candidateDelimiter
      ? `%${character.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0")}`
      : character;
  }).join("");
  return context.syntax === "markdown"
    && (context.raw.startsWith("<") || /[\s()]/.test(encoded))
    ? `<${encoded}>`
    : encoded;
}

function stripLabelBrackets(value: string): string {
  return value.startsWith("[") && value.endsWith("]")
    ? value.slice(1, -1)
    : value;
}

function normalizeReferenceLabel(value: string): string {
  // Labels match as source text; escapes and entities are only decoded in URLs.
  // UniCase's default Unicode folding keeps dotless i distinct from I/i.
  // Preserve it while expanding other case variants such as sharp s and sigma.
  return value
    .replace(/[\t\n\r ]+/g, " ")
    .replace(/^ | $/g, "")
    .replace(/[^\u0131]+/gu, part => part.toLowerCase().toUpperCase());
}
