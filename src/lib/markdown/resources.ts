import { decodeHTMLAttribute, decodeHTMLStrict } from "entities/decode";

const remoteFetchAttributes = [
  ["img", "src"],
  ["img", "srcset"],
  ["source", "src"],
  ["source", "srcset"],
  ["video", "src"],
  ["video", "poster"],
  ["audio", "src"],
  ["track", "src"],
  ["iframe", "src"],
  ["embed", "src"],
  ["input", "src"],
  ["object", "data"],
  ["link", "href"],
  ["image", "href"],
  ["image", "xlink:href"],
  ["use", "href"],
  ["use", "xlink:href"],
] as const;

export interface ImageSrcsetCandidate {
  source: string;
  descriptor: string;
}

const supportedResponsiveImageTypes = new Set([
  "image/apng",
  "image/avif",
  "image/bmp",
  "image/gif",
  "image/jpeg",
  "image/png",
  "image/svg+xml",
  "image/vnd.microsoft.icon",
  "image/webp",
  "image/x-icon",
]);

export function hasUsableResponsiveImageSource(
  image: HTMLImageElement,
): boolean {
  if (image.getAttribute("srcset")?.trim()) return true;
  const picture = image.parentElement;
  if (picture?.tagName !== "PICTURE") return false;
  return Array.from(picture.children).some((child) => {
    if (!(child instanceof HTMLSourceElement)) return false;
    if (!child.getAttribute("srcset")?.trim()) return false;
    if (!responsiveImageTypeIsSupported(child.getAttribute("type"))) return false;
    return responsiveImageMediaMatches(child.getAttribute("media"));
  });
}

/**
 * Returns whether the sanitized document still contains a responsive source
 * for this image, without resolving viewport-dependent media queries. Exported
 * HTML can be opened in a different viewport, so its picture structure must not
 * be reduced according to the editor or hidden renderer's current dimensions.
 */
export function hasRetainedResponsiveImageSource(
  image: HTMLImageElement,
): boolean {
  if (image.getAttribute("srcset")?.trim()) return true;
  const picture = image.parentElement;
  if (picture?.tagName !== "PICTURE") return false;
  return Array.from(picture.children).some((child) =>
    child instanceof HTMLSourceElement
    && Boolean(child.getAttribute("srcset")?.trim())
  );
}

function responsiveImageTypeIsSupported(type: string | null): boolean {
  if (!type?.trim()) return true;
  const mimeType = type.split(";", 1)[0].trim().toLowerCase();
  return supportedResponsiveImageTypes.has(mimeType);
}

function responsiveImageMediaMatches(media: string | null): boolean {
  if (!media?.trim()) return true;
  if (typeof window.matchMedia !== "function") return false;
  try {
    return window.matchMedia(media).matches;
  } catch {
    return false;
  }
}

export async function hasRemoteImages(markdown: string): Promise<boolean> {
  if (!mayContainImageResources(markdown)) return false;
  const { renderMarkdownForResourceDetection } = await import("./pipeline");
  const html = await renderMarkdownForResourceDetection(markdown);
  return await hasRemoteImagesInRenderedHtml(html);
}

export function mayContainImageResources(markdown: string): boolean {
  return /!\[|<(?:img|source)\b|(?:["']img["']|\bimg)\s*:|@\{/i.test(markdown);
}

export async function hasRemoteImagesInRenderedHtml(html: string): Promise<boolean> {
  if (hasRemoteFetchInImageTags(html)) return true;

  for (const match of html.matchAll(/<code\b([^>]*)>([\s\S]*?)<\/code>/gi)) {
    const classMatch = /(?:^|\s)class\s*=\s*(?:"([^"]*)"|'([^']*)')/i.exec(match[1]);
    const classes = decodeHTMLAttribute(classMatch?.[1] ?? classMatch?.[2] ?? "");
    if (!/(?:^|\s)language-mermaid(?:\s|$)/i.test(classes)) continue;
    if (await hasRemoteMermaidImageReference(decodeHTMLStrict(match[2]))) return true;
  }
  return false;
}

export async function hasRemoteMermaidImageReference(source: string): Promise<boolean> {
  const decodedSource = decodeHTMLStrict(source);
  const normalizedSource = decodeMermaidStringEscapes(decodedSource);
  if (hasRemoteFetchInImageTags(normalizedSource)) return true;

  for (
    const match of normalizedSource.matchAll(
      /(?:^|[,\{\s])(?:img|icon|"img"|"icon"|'img'|'icon')\s*:\s*(?:"((?:\\.|[^"\\])*)"|'((?:\\.|[^'\\])*)'|([^,}\]\s]+))/gi,
    )
  ) {
    const value = match[1] ?? match[2] ?? match[3] ?? "";
    if (isRemoteImageSource(value)) return true;
  }

  for (const match of normalizedSource.matchAll(/!\[[^\]]*\]\(\s*(?:<([^>\r\n]+)>|([^\s)\r\n]+))/g)) {
    const value = decodeMarkdownResourceDestination(match[1] ?? match[2] ?? "");
    if (isRemoteImageSource(value)) return true;
  }

  const metadataBlocks = extractMermaidMetadataBlocks(decodedSource);
  if (metadataBlocks.length === 0) return false;

  const { JSON_SCHEMA, load } = await import("js-yaml");
  for (const { content: metadata } of metadataBlocks) {
    try {
      // Mermaid wraps single-line metadata as a flow mapping, but sends
      // multiline metadata directly to js-yaml as a block mapping.
      const yaml = metadata.includes("\n")
        ? `${metadata}\n`
        : `{\n${metadata}\n}`;
      const parsed = load(yaml, { schema: JSON_SCHEMA });
      if (!isRecord(parsed)) continue;
      for (const property of ["img", "icon"] as const) {
        if (!Object.prototype.hasOwnProperty.call(parsed, property)) continue;
        const imageSource = parsed[property];
        if (imageSource && isRemoteImageSource(String(imageSource))) return true;
      }
    } catch {
      // Mermaid uses the same parser and will normally reject this metadata too.
      // Fail closed for an image declaration in case its accepted syntax changes.
      if (/(?:^|[,\s])(?:img|icon|"img"|"icon"|'img'|'icon')\s*:/i.test(metadata)) {
        return true;
      }
    }
  }
  return false;
}

export async function resolveLocalMermaidImageReferences(
  source: string,
  loadResource?: (source: string) => Promise<string>,
): Promise<string> {
  if (!loadResource || !source.includes("@{")) return source;

  const blocks = extractMermaidMetadataBlocks(source);
  if (blocks.length === 0) return source;

  const { JSON_SCHEMA, dump, load } = await import("js-yaml");
  const sequenceDiagram = /(?:^|\r?\n)\s*sequenceDiagram\b/i.test(source);
  let rewritten = "";
  let sourceOffset = 0;

  for (const block of blocks) {
    let parsed: unknown;
    try {
      // Mermaid wraps single-line metadata as a flow mapping, but parses a
      // multiline block directly. Parsing the complete mapping is required for
      // aliases and block scalars; parsing only the img token can silently miss
      // valid YAML and leak the original local path to Mermaid.
      const yaml = block.content.includes("\n")
        ? `${block.content}\n`
        : `{\n${block.content}\n}`;
      parsed = load(yaml, { schema: JSON_SCHEMA });
    } catch (error) {
      if (/(?:^|[,\s])(?:img|icon|"img"|"icon"|'img'|'icon')\s*:/i.test(block.content)) {
        throw new Error(
          "Mermaid image metadata could not be safely resolved.",
          { cause: error },
        );
      }
      continue;
    }
    if (!isRecord(parsed)) continue;
    let changed = false;
    for (const property of ["img", "icon"] as const) {
      if (!Object.prototype.hasOwnProperty.call(parsed, property)) continue;
      const resource = parsed[property];
      if (
        typeof resource !== "string"
        || !resource.trim()
        || isRemoteImageSource(resource)
        || /^(?:data:|blob:)/i.test(resource.trim())
        || (property === "icon" && isMermaidIconIdentifier(resource, !sequenceDiagram))
      ) {
        continue;
      }

      // Sequence-diagram icon values that do not begin with `@` are image
      // URLs. Resolve path-shaped values before Mermaid can append them to its
      // temporary live SVG and trigger an app-origin request.
      parsed[property] = await loadResource(resource);
      changed = true;
    }
    if (!changed) continue;

    // Never pass an unresolved local path to Mermaid. Mermaid may interpret
    // it as a fetchable app-origin/asset URL, bypassing the document-scoped
    // resource loader. Callers keep the inert source block when this fails.
    const normalized = dump(parsed, {
      schema: JSON_SCHEMA,
      // Preserve shared and cyclic metadata instead of recursively copying it:
      // a small alias graph can otherwise expand exponentially on the UI thread.
      noRefs: false,
      lineWidth: -1,
    }).trimEnd();

    rewritten += source.slice(sourceOffset, block.start);
    rewritten += `\n${normalized}\n`;
    sourceOffset = block.end;
  }

  return sourceOffset === 0 ? source : rewritten + source.slice(sourceOffset);
}

interface MermaidMetadataBlock {
  content: string;
  start: number;
  end: number;
}

function extractMermaidMetadataBlocks(source: string): MermaidMetadataBlock[] {
  const blocks: MermaidMetadataBlock[] = [];
  let searchFrom = 0;
  while (searchFrom < source.length) {
    const start = source.indexOf("@{", searchFrom);
    if (start < 0) break;
    let depth = 1;
    let quote: "\"" | "'" | null = null;
    let escaped = false;
    let index = start + 2;
    for (; index < source.length; index += 1) {
      const character = source[index];
      if (quote === "\"") {
        if (escaped) escaped = false;
        else if (character === "\\") escaped = true;
        else if (character === "\"") quote = null;
        continue;
      }
      if (quote === "'") {
        if (character !== "'") continue;
        if (source[index + 1] === "'") index += 1;
        else quote = null;
        continue;
      }
      if (character === "\"" || character === "'") quote = character;
      else if (character === "{") depth += 1;
      else if (character === "}" && --depth === 0) break;
    }
    if (depth === 0) {
      blocks.push({
        content: source.slice(start + 2, index),
        start: start + 2,
        end: index,
      });
    }
    searchFrom = depth === 0 ? index + 1 : start + 2;
  }
  return blocks;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isMermaidIconIdentifier(value: string, allowIconify: boolean): boolean {
  const normalized = value.trim();
  return normalized.startsWith("@")
    || (
      allowIconify
      && /^[a-z0-9]+(?:-[a-z0-9]+)*:[a-z0-9]+(?:-[a-z0-9]+)*$/i.test(normalized)
    );
}

function decodeMermaidStringEscapes(source: string): string {
  return source
    .replace(/\\x([\da-fA-F]{2})/g, (_, hex: string) =>
      String.fromCodePoint(Number.parseInt(hex, 16)))
    .replace(/\\u([\da-fA-F]{4})/g, (_, hex: string) =>
      String.fromCodePoint(Number.parseInt(hex, 16)))
    .replace(/\\U([\da-fA-F]{8})/g, (_, hex: string) => {
      const codePoint = Number.parseInt(hex, 16);
      return codePoint <= 0x10ffff ? String.fromCodePoint(codePoint) : "";
    });
}

function hasRemoteFetchInImageTags(html: string): boolean {
  for (
    const tagMatch of html.matchAll(
      /<(?:img|source)\b(?:[^>"']|"[^"]*"|'[^']*')*>/gi,
    )
  ) {
    const tag = tagMatch[0];
    for (
      const match of tag.matchAll(
        /(?:^|\s)(src|srcset)\s*=\s*(?:(["'])([\s\S]*?)\2|([^\s"'=<>`]+))/gi,
      )
    ) {
      const value = decodeHTMLAttribute(match[3] ?? match[4] ?? "");
      if (attributeContainsRemoteUrl(match[1].toLowerCase(), value)) {
        return true;
      }
    }
  }
  return false;
}

export function decodeMarkdownResourceDestination(source: string): string {
  return decodeHTMLStrict(
    source.replace(
      /\\([!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~])/g,
      "$1",
    ),
  );
}

export function isRemoteImageSource(source: string): boolean {
  const normalized = source
    // URL parsing strips leading and trailing C0 controls and spaces before
    // resolving the scheme. Apply the same preprocessing before the security check.
    .replace(/^[\u0000-\u0020]+|[\u0000-\u0020]+$/g, "")
    .replace(/[\t\n\r]/g, "")
    .replace(/\\/g, "/");
  if (!/^(?:https?:|\/\/)/i.test(normalized)) return false;
  try {
    const base = typeof document === "undefined" || !document.baseURI
      ? "http://localhost/"
      : document.baseURI;
    const resolved = new URL(normalized, base);
    return resolved.protocol === "http:" || resolved.protocol === "https:";
  } catch {
    // An invalid explicit network reference must stay inert instead of reaching the DOM.
    return true;
  }
}

export function blockRemoteImageRequests(html: string): string {
  const template = document.createElement("template");
  template.innerHTML = html;
  for (const [tagName, attribute] of remoteFetchAttributes) {
    for (const element of Array.from(template.content.querySelectorAll(tagName))) {
      if (!element.hasAttribute(attribute)) continue;
      const value = element.getAttribute(attribute) ?? "";
      if (!attributeContainsRemoteUrl(attribute, value)) continue;
      const storageAttribute = attribute.replace(":", "-");
      if (attribute === "srcset") {
        const candidates = parseImageSrcset(value);
        const localCandidates = candidates.filter(
          ({ source }) => !isRemoteImageSource(source),
        );
        const remoteCandidates = candidates.filter(({ source }) =>
          isRemoteImageSource(source)
        );
        element.setAttribute(
          `data-inkflow-remote-${storageAttribute}`,
          serializeImageSrcset(remoteCandidates),
        );
        if (localCandidates.length > 0) {
          element.setAttribute(attribute, serializeImageSrcset(localCandidates));
          element.classList.add("remote-partially-blocked");
        } else {
          element.removeAttribute(attribute);
          const hasLocalFallback = element instanceof HTMLImageElement
            && Boolean(element.getAttribute("src")?.trim());
          if (
            element instanceof HTMLImageElement
            && !hasLocalFallback
            && !element.hasAttribute("data-inkflow-remote-src")
          ) {
            const blockedSource = remoteCandidates[0]?.source;
            if (blockedSource) {
              // Preview and export share this marker for an image whose only
              // usable candidates were removed. Keep the full responsive list
              // above as metadata, but expose one source for the fallback label.
              element.setAttribute("data-inkflow-remote-src", blockedSource);
            }
          }
          element.classList.toggle("remote-blocked", !hasLocalFallback);
          element.classList.toggle("remote-partially-blocked", hasLocalFallback);
        }
      } else {
        element.setAttribute(`data-inkflow-remote-${storageAttribute}`, value);
        element.removeAttribute(attribute);
        element.classList.add("remote-blocked");
      }
    }
  }
  return template.innerHTML;
}

function attributeContainsRemoteUrl(attribute: string, value: string): boolean {
  if (attribute === "srcset") {
    return parseImageSrcset(value).some(({ source }) =>
      isRemoteImageSource(source)
    );
  }
  return isRemoteImageSource(value);
}

// Covers the useful part of the HTML srcset parsing algorithm. Commas may be
// part of a URL (notably data URLs), while a trailing comma or a comma after a
// descriptor terminates a candidate.
export function parseImageSrcset(value: string): ImageSrcsetCandidate[] {
  const candidates: ImageSrcsetCandidate[] = [];
  let index = 0;
  const isWhitespace = (character: string) => /[\t\n\f\r ]/.test(character);

  while (index < value.length) {
    while (
      index < value.length &&
      (isWhitespace(value[index]) || value[index] === ",")
    ) {
      index += 1;
    }
    if (index >= value.length) break;

    const sourceStart = index;
    while (index < value.length && !isWhitespace(value[index])) index += 1;
    let source = value.slice(sourceStart, index);
    let endedWithComma = false;
    while (source.endsWith(",")) {
      source = source.slice(0, -1);
      endedWithComma = true;
    }
    if (!source) continue;
    if (endedWithComma) {
      candidates.push({ source, descriptor: "" });
      continue;
    }

    while (index < value.length && isWhitespace(value[index])) index += 1;
    const descriptorStart = index;
    let parentheses = 0;
    while (index < value.length) {
      if (value[index] === "(") parentheses += 1;
      else if (value[index] === ")" && parentheses > 0) parentheses -= 1;
      else if (value[index] === "," && parentheses === 0) break;
      index += 1;
    }
    const descriptor = value.slice(descriptorStart, index).trim();
    if (index < value.length && value[index] === ",") index += 1;
    candidates.push({ source, descriptor });
  }

  return candidates;
}

export function serializeImageSrcset(
  candidates: ImageSrcsetCandidate[],
): string {
  return candidates
    .map(({ source, descriptor }) =>
      descriptor ? `${source} ${descriptor}` : source,
    )
    .join(", ");
}
