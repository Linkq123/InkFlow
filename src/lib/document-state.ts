import type { DocumentTab, SaveOutcome } from "./api/types";

const imagePattern = /(!\[[^\]\r\n]*\]\()(<[^>\r\n]+>|[^\s)\r\n]+)((?:\s+["'][^)\r\n]*["'])?\))/g;
const htmlImagePattern = /(<img\b[^>\r\n]*?\bsrc\s*=\s*["'])([^"'\r\n]+)(["'][^>]*>)/gi;
const definitionPattern = /(^\s{0,3}\[([^\]\r\n]+)\]:\s*)(<[^>\r\n]+>|[^<\s\r\n]+)(.*$)/gm;

function normalizeLabel(value: string): string {
  return value.trim().replace(/\s+/g, " ").toLocaleLowerCase();
}

function imageReferenceLabels(content: string): Set<string> {
  const labels = new Set<string>();
  for (const match of content.matchAll(/!\[([^\]\r\n]*)\]\[([^\]\r\n]*)\]/g)) {
    labels.add(normalizeLabel(match[2] || match[1]));
  }
  for (const match of content.matchAll(/!\[([^\]\r\n]+)\]/g)) {
    const next = content[match.index + match[0].length];
    if (next !== "(" && next !== "[") labels.add(normalizeLabel(match[1]));
  }
  return labels;
}

function mergeImageRewrites(current: string, saved: string, rewritten: string): string {
  const rewrites = new Map<string, string>();
  const collect = (pattern: RegExp, beforeIndex: number, afterIndex: number, predicate?: (match: RegExpMatchArray) => boolean) => {
    const before = [...saved.matchAll(pattern)].filter((match) => !predicate || predicate(match));
    const after = [...rewritten.matchAll(pattern)].filter((match) => !predicate || predicate(match));
    if (before.length !== after.length) return;
    before.forEach((match, index) => {
      if (match[beforeIndex] !== after[index][afterIndex]) rewrites.set(match[beforeIndex], after[index][afterIndex]);
    });
  };
  collect(imagePattern, 2, 2);
  collect(htmlImagePattern, 2, 2);
  const savedLabels = imageReferenceLabels(saved);
  const rewrittenLabels = imageReferenceLabels(rewritten);
  collect(definitionPattern, 3, 3, (match) => savedLabels.has(normalizeLabel(match[2])) || rewrittenLabels.has(normalizeLabel(match[2])));
  if (!rewrites.size) return current;
  let merged = current.replace(imagePattern, (match, prefix: string, destination: string, suffix: string) => {
    const replacement = rewrites.get(destination);
    return replacement ? `${prefix}${replacement}${suffix}` : match;
  });
  merged = merged.replace(htmlImagePattern, (match, prefix: string, destination: string, suffix: string) => {
    const replacement = rewrites.get(destination);
    return replacement ? `${prefix}${replacement}${suffix}` : match;
  });
  const currentLabels = imageReferenceLabels(current);
  return merged.replace(definitionPattern, (match, prefix: string, label: string, destination: string, suffix: string) => {
    const replacement = currentLabels.has(normalizeLabel(label)) ? rewrites.get(destination) : undefined;
    return replacement ? `${prefix}${replacement}${suffix}` : match;
  });
}

export function applySavedResult(
  tab: DocumentTab,
  result: Extract<SaveOutcome, { status: "saved" }>,
  savedContent: string,
): { tab: DocumentTab; needsResave: boolean } {
  const changedDuringSave = tab.content !== savedContent;
  const content = result.content
    ? changedDuringSave
      ? mergeImageRewrites(tab.content, savedContent, result.content)
      : result.content
    : tab.content;
  return {
    tab: {
      ...tab,
      path: result.path,
      content,
      revision: result.revision,
      dirty: changedDuringSave,
      saveState: changedDuringSave ? "dirty" : "saved",
      externalChange: null,
    },
    needsResave: changedDuringSave,
  };
}

export function isPathAffected(path: string | null, entryPath: string, isDirectory: boolean): boolean {
  if (!path) return false;
  const normalize = (value: string) => value.replace(/\//g, "\\").replace(/\\+$/, "").toLocaleLowerCase();
  const candidate = normalize(path);
  const entry = normalize(entryPath);
  return candidate === entry || (isDirectory && candidate.startsWith(`${entry}\\`));
}

export function relocatedPath(path: string, source: string, destination: string, isDirectory: boolean): string {
  if (!isPathAffected(path, source, isDirectory)) return path;
  return `${destination}${path.slice(source.length)}`;
}

export function replaceUploadPlaceholder(content: string, placeholder: string, replacement: string): string | null {
  if (!content.includes(placeholder)) return null;
  return content.replace(placeholder, replacement);
}

export function withoutTabsById(tabs: DocumentTab[], ids: Iterable<string>): DocumentTab[] {
  const removed = new Set(ids);
  return tabs.filter((tab) => !removed.has(tab.id));
}
