import { commonmarkLanguage } from "@codemirror/lang-markdown";
import { decodeHTMLStrict } from "entities/decode";

export interface ImageDestination {
  raw: string;
  /** Markup escapes/entities decoded; URL percent escapes remain untouched. */
  destination: string;
  from: number;
  to: number;
  syntax: "markdown" | "html";
  quote?: string | null;
  attribute?: "src" | "srcset";
}

const asciiWhitespace = (value: string) => /[\t\n\v\f\r ]/.test(value);

// Keep these source ranges aligned with asset.rs. DOM parsing loses original
// quoting/offsets, and a quoted-src regex misses responsive and unquoted images.
function htmlImageDestinations(source: string, offset: number): ImageDestination[] {
  const result: ImageDestination[] = [];
  let cursor = 0;
  while (cursor < source.length) {
    const start = source.indexOf("<", cursor);
    if (start < 0) break;
    if (source.startsWith("<!--", start)) {
      const end = source.indexOf("-->", start + 4);
      cursor = end < 0 ? source.length : end + 3;
      continue;
    }
    const tag = /^(img|source)(?=[\t\n\v\f\r />])/i.exec(source.slice(start + 1));
    cursor = start + 1;
    if (!tag) continue;
    cursor += tag[0].length;
    let end = cursor;
    let activeQuote: string | null = null;
    for (; end < source.length; end++) {
      const character = source[end];
      if (activeQuote) {
        if (character === activeQuote) activeQuote = null;
      } else if (character === "'" || character === '"') activeQuote = character;
      else if (character === ">") break;
    }
    if (end === source.length) break;
    while (cursor < end) {
      while (cursor < end && asciiWhitespace(source[cursor])) cursor++;
      if (cursor >= end || source[cursor] === "/") break;
      const nameStart = cursor;
      while (cursor < end && !asciiWhitespace(source[cursor]) && !"/=>".includes(source[cursor])) cursor++;
      if (cursor === nameStart) { cursor++; continue; }
      const name = source.slice(nameStart, cursor).toLowerCase();
      while (cursor < end && asciiWhitespace(source[cursor])) cursor++;
      if (source[cursor] !== "=") continue;
      cursor++;
      while (cursor < end && asciiWhitespace(source[cursor])) cursor++;
      const quote = source[cursor] === "'" || source[cursor] === '"' ? source[cursor++] : null;
      const valueStart = cursor;
      while (cursor < end && (quote ? source[cursor] !== quote : !asciiWhitespace(source[cursor]))) cursor++;
      const valueEnd = cursor;
      if (quote && cursor < end) cursor++;
      if (name !== "srcset" && !(tag[1].toLowerCase() === "img" && name === "src")) continue;
      const ranges = name === "srcset"
        ? srcsetRanges(source.slice(valueStart, valueEnd)).map(({ from, to }) => ({ from: from + valueStart, to: to + valueStart }))
        : [{ from: valueStart, to: valueEnd }];
      for (const { from, to } of ranges) {
        const raw = source.slice(from, to);
        result.push({
          raw, destination: decodeHTMLStrict(raw), from: offset + from, to: offset + to,
          syntax: "html", quote, attribute: name === "srcset" ? "srcset" : "src",
        });
      }
    }
    cursor = end + 1;
  }
  return result;
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
  const destinations: ImageDestination[] = [];
  const referenceLabels = new Set<string>();
  const referenceDefinitions = new Map<string, ImageDestination>();
  const htmlRanges: Array<{ from: number; to: number }> = [];
  const tree = commonmarkLanguage.parser.parse(markdown);

  tree.iterate({
    enter(node) {
      if (node.name === "Image") {
        const url = node.node.getChild("URL");
        if (url) {
          destinations.push(markdownDestination(markdown, url.from, url.to));
          return;
        }

        const marks = node.node.getChildren("LinkMark");
        if (marks.length < 2) return;
        const alt = markdown.slice(marks[0].to, marks[1].from);
        const labelNode = node.node.getChild("LinkLabel");
        const explicitLabel = labelNode
          ? stripLabelBrackets(markdown.slice(labelNode.from, labelNode.to))
          : "";
        referenceLabels.add(normalizeReferenceLabel(explicitLabel || alt));
        return;
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

  for (const range of htmlRanges) {
    const source = markdown.slice(range.from, range.to);
    destinations.push(...htmlImageDestinations(source, range.from));
  }

  destinations.sort((left, right) => left.from - right.from || left.to - right.to);
  return destinations.filter((destination, index) => {
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

function decodeMarkdownDestination(value: string): string {
  // Decode escapes and character references in one pass: `\&amp;` denotes the
  // literal string `&amp;`, not an ampersand character reference.
  return value.replace(
    /\\([!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~])|&(?:#[xX][0-9a-fA-F]{1,6}|#[0-9]{1,7}|[A-Za-z][A-Za-z0-9]{1,31});/g,
    (match, escaped: string | undefined) => escaped ?? decodeHTMLStrict(match),
  );
}

/** Encode a decoded, backend-generated asset path in the current source syntax. */
export function encodeImageDestinationPath(path: string, context: ImageDestination): string {
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
  return decodeHTMLStrict(
    value.replace(
      /\\([!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~])/g,
      "$1",
    ),
  )
    .trim()
    .replace(/\s+/g, " ")
    .toLocaleLowerCase();
}
