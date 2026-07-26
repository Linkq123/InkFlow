import type { DocumentTab, SaveOutcome } from "./api/types";
import { collectImageDestinations } from "./markdown/image-destinations";

export interface TextEdit {
  from: number;
  to: number;
  insert: string;
}

function imageRewriteMap(saved: string, rewritten: string): Map<string, string> {
  const rewrites = new Map<string, string>();
  const before = collectImageDestinations(saved);
  const after = collectImageDestinations(rewritten);
  if (before.length !== after.length) return rewrites;
  before.forEach((destination, index) => {
    const next = after[index];
    if (
      destination.syntax === next.syntax
      && destination.raw !== next.raw
    ) {
      rewrites.set(destination.raw, next.raw);
    }
  });
  return rewrites;
}

export function imageRewriteEdits(
  current: string,
  saved: string,
  rewritten: string,
): TextEdit[] {
  const rewrites = imageRewriteMap(saved, rewritten);
  if (!rewrites.size) return [];
  return collectImageDestinations(current)
    .flatMap((destination): TextEdit[] => {
      const replacement = rewrites.get(destination.raw);
      return replacement && replacement !== destination.raw
        ? [{
            from: destination.from,
            to: destination.to,
            insert: replacement,
          }]
        : [];
    });
}

export function imageRewriteEditsBetween(before: string, after: string): TextEdit[] {
  const beforeDestinations = collectImageDestinations(before);
  const afterDestinations = collectImageDestinations(after);
  if (beforeDestinations.length !== afterDestinations.length) return [];
  const edits = beforeDestinations.flatMap((destination, index): TextEdit[] => {
    const next = afterDestinations[index];
    return destination.syntax === next.syntax && destination.raw !== next.raw
      ? [{
          from: destination.from,
          to: destination.to,
          insert: next.raw,
        }]
      : [];
  });

  const ordered = edits.sort((left, right) => left.from - right.from);
  const normalized: TextEdit[] = [];
  for (const edit of ordered) {
    const previous = normalized.at(-1);
    if (
      previous
      && previous.from === edit.from
      && previous.to === edit.to
      && previous.insert === edit.insert
    ) {
      continue;
    }
    if (previous && edit.from < previous.to) return [];
    normalized.push(edit);
  }
  return applyTextEdits(before, normalized) === after ? normalized : [];
}

export function mergeImageRewrites(current: string, saved: string, rewritten: string): string {
  return applyTextEdits(current, imageRewriteEdits(current, saved, rewritten));
}

export function applyTextEdits(current: string, edits: readonly TextEdit[]): string {
  if (!edits.length) return current;
  const parts: string[] = [];
  let cursor = 0;
  for (const edit of edits) {
    if (edit.from < cursor || edit.to < edit.from || edit.to > current.length) {
      throw new RangeError("Text edits must be ordered, non-overlapping, and in range.");
    }
    parts.push(current.slice(cursor, edit.from), edit.insert);
    cursor = edit.to;
  }
  parts.push(current.slice(cursor));
  return parts.join("");
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
  const contentChanged = content !== tab.content;
  return {
    tab: {
      ...tab,
      path: result.path,
      content,
      editorVersion: contentChanged ? tab.editorVersion + 1 : tab.editorVersion,
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
  const edit = uploadPlaceholderEdit(content, placeholder, replacement);
  return edit ? applyTextEdits(content, [edit]) : null;
}

export function uploadPlaceholderEdit(
  content: string,
  placeholder: string,
  replacement: string,
): TextEdit | null {
  const from = content.indexOf(placeholder);
  return from < 0
    ? null
    : { from, to: from + placeholder.length, insert: replacement };
}

export function withoutTabsById(tabs: DocumentTab[], ids: Iterable<string>): DocumentTab[] {
  const removed = new Set(ids);
  return tabs.filter((tab) => !removed.has(tab.id));
}
