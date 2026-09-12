import { constructFromEvents, EVENT_ID, getScalarValue, JSON_SCHEMA, parseEvents, SCALAR_STYLE, type ScalarEvent } from "js-yaml";

// Keep these limits and source rules in sync with mermaid_assets.rs.
export const MAX_MERMAID_SOURCE = 1024 * 1024;
export const MAX_MERMAID_DEPTH = 64;
export interface MermaidMetadataBlock { content: string; start: number; end: number }
export interface MermaidAliasEdit { from: number; to: number; insert: string }
export interface MermaidImageReference {
  source: string; from: number; to: number; trailingNewline: string;
  preservedAlias?: MermaidAliasEdit;
}

/** Each character is visited at most once, including malformed trailing blocks. */
export function extractMermaidMetadataBlocks(source: string): MermaidMetadataBlock[] {
  if (source.length > MAX_MERMAID_SOURCE) throw new Error("Mermaid source exceeds the resource parsing limit.");
  const blocks: MermaidMetadataBlock[] = [];
  let cursor = 0;
  while (cursor < source.length) {
    const start = source.indexOf("@{", cursor);
    if (start < 0) break;
    let depth = 1;
    let nesting = 1;
    let quote: string | null = null;
    let escaped = false;
    let index = start + 2;
    for (; index < source.length; index++) {
      const character = source[index];
      if (quote === '"') {
        if (escaped) escaped = false;
        else if (character === "\\") escaped = true;
        else if (character === '"') quote = null;
        continue;
      }
      if (quote === "'") {
        if (character === "'") {
          if (source[index + 1] === "'") index++;
          else quote = null;
        }
        continue;
      }
      if (character === "#" && /\s/.test(source[index - 1] ?? " ")) {
        const end = source.indexOf("\n", index);
        index = end < 0 ? source.length : end;
      } else if (character === '"' || character === "'") quote = character;
      else if (character === "{" || character === "[") {
        if (character === "{") depth++;
        if (++nesting > MAX_MERMAID_DEPTH) throw new Error("Mermaid metadata nesting is too deep.");
      } else if (character === "}" || character === "]") {
        nesting = Math.max(0, nesting - 1);
        if (character === "}" && --depth === 0) break;
      }
    }
    if (depth !== 0) break;
    blocks.push({ content: source.slice(start + 2, index), start: start + 2, end: index });
    cursor = index + 1;
  }
  return blocks;
}

function scalarRange(yaml: string, event: ScalarEvent): { from: number; to: number; trailingNewline: string } {
  if (event.style === SCALAR_STYLE.SINGLE_QUOTED || event.style === SCALAR_STYLE.DOUBLE_QUOTED) {
    return { from: event.valueStart - 1, to: event.valueEnd + 1, trailingNewline: "" };
  }
  if (event.style === SCALAR_STYLE.LITERAL_BLOCK || event.style === SCALAR_STYLE.FOLDED_BLOCK) {
    let lineEnd = yaml.lastIndexOf("\n", event.valueStart - 1);
    while (lineEnd >= 0) {
      const lineStart = yaml.lastIndexOf("\n", lineEnd - 1) + 1;
      const header = /[|>](?:[+-]?[1-9]?|[1-9][+-]?)[ \t]*(?:#.*)?$/.exec(yaml.slice(lineStart, lineEnd).replace(/\r$/, ""));
      if (header) return { from: lineStart + header.index, to: event.valueEnd, trailingNewline: "\n" };
      lineEnd = lineStart - 1;
    }
    throw new Error("Unable to locate Mermaid block scalar.");
  }
  return { from: event.valueStart, to: event.valueEnd, trailingNewline: "" };
}

/** Read only root image properties; never expand YAML alias graphs. */
export function collectMermaidImageReferences(source: string): MermaidImageReference[] {
  const references: MermaidImageReference[] = [];
  const sequence = /(?:^|\r?\n)\s*sequenceDiagram\b/i.test(source);
  for (const block of extractMermaidMetadataBlocks(source)) {
    const prefix = block.content.includes("\n") ? "" : "{\n";
    const yaml = `${prefix}${block.content}\n${prefix ? "}" : ""}`;
    try {
      const events = parseEvents(yaml, { maxDepth: MAX_MERMAID_DEPTH });
      const documents = constructFromEvents(events, { source: yaml, schema: JSON_SCHEMA, maxAliases: 10_000 });
      const parsed = documents[0];
      if (documents.length !== 1 || !parsed || typeof parsed !== "object" || Array.isArray(parsed)) continue;
      const anchors = new Map<string, { value: string; image?: MermaidImageReference }>();
      let depth = 0;
      let key: string | null = null;
      let expectingKey = true;
      for (const event of events) {
        if (event.type === EVENT_ID.POP) { depth--; continue; }
        if (event.type === EVENT_ID.DOCUMENT) { depth++; continue; }
        const scalar = event.type === EVENT_ID.SCALAR ? getScalarValue(yaml, event) : undefined;
        const anchorName = event.anchorStart >= 0 ? yaml.slice(event.anchorStart, event.anchorEnd) : "";
        if (event.type !== EVENT_ID.ALIAS && anchorName) {
          // A later declaration shadows the old binding, including collections.
          if (scalar !== undefined) anchors.set(anchorName, { value: scalar });
          else anchors.delete(anchorName);
        }
        const anchor = anchors.get(anchorName);
        const value = event.type === EVENT_ID.ALIAS ? anchor?.value : scalar;
        let image: MermaidImageReference | undefined;
        if (depth === 2) {
          if (expectingKey) key = value ?? null;
          else if ((key === "img" || key === "icon") && (event.type === EVENT_ID.SCALAR || event.type === EVENT_ID.ALIAS)) {
            const resource = (parsed as Record<string, unknown>)[key];
            if (typeof resource === "string" && resource.trim() && !(key === "icon" && isMermaidIconIdentifier(resource, !sequence))) {
              const range = event.type === EVENT_ID.SCALAR ? scalarRange(yaml, event)
                : { from: event.anchorStart - 1, to: event.anchorEnd, trailingNewline: "" };
              const from = block.start + range.from - prefix.length;
              const to = Math.min(block.end, block.start + range.to - prefix.length);
              if (from >= block.start && to >= from) {
                image = { source: resource, from, to, trailingNewline: range.trailingNewline };
                references.push(image);
                if (event.type === EVENT_ID.SCALAR && anchor) anchor.image = image;
              }
            }
          }
          expectingKey = !expectingKey;
        }
        if (event.type === EVENT_ID.ALIAS && !image && anchor?.image && !anchor.image.preservedAlias) {
          // Rebind at the first non-image use. All later aliases keep the old
          // value without expanding a shared/cyclic auxiliary metadata graph.
          anchor.image.preservedAlias = {
            from: block.start + event.anchorStart - 1 - prefix.length,
            to: block.start + event.anchorEnd - prefix.length,
            insert: `&${anchorName} ${JSON.stringify(anchor.value)}`,
          };
        }
        if (event.type === EVENT_ID.MAPPING || event.type === EVENT_ID.SEQUENCE) depth++;
      }
    } catch (error) {
      if (/(?:^|[,\s])(?:img|icon|"img"|"icon"|'img'|'icon')\s*:/i.test(block.content)) {
        throw new Error("Mermaid image metadata could not be safely resolved.", { cause: error });
      }
    }
  }
  return references;
}

function isMermaidIconIdentifier(value: string, allowIconify: boolean): boolean {
  const normalized = value.trim();
  return normalized.startsWith("@") || (allowIconify && /^[a-z0-9]+(?:-[a-z0-9]+)*:[a-z0-9]+(?:-[a-z0-9]+)*$/i.test(normalized));
}

export function encodeMermaidImageReference(path: string, trailingNewline = ""): string {
  return JSON.stringify(path) + trailingNewline;
}
