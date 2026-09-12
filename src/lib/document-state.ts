import type { DocumentTab, SaveOutcome } from "./api/types";
import { Text } from "@codemirror/state";
import { collectImageDestinations, encodeImageDestinationPath, type ImageDestination } from "./markdown/image-destinations";
import { parseImageDestinations } from "./markdown/image-destination-service";
import type { WorkCheckpoint } from "./async";

export interface TextEdit {
  from: number;
  to: number;
  insert: string;
}

export function imagePathRewriteEdits(
  current: string,
  rewrites: ReadonlyArray<{ source: string; destination: string }>,
  destinations = collectImageDestinations(current),
): TextEdit[] {
  const paths = new Map(rewrites.map(({ source, destination }) => [source, decodeURIComponent(destination)]));
  return destinations.flatMap((destination): TextEdit[] => {
    const target = paths.get(destination.destination);
    if (target === undefined) return [];
    const insert = encodeImageDestinationPath(target, destination);
    return insert === destination.raw ? [] : [{ from: destination.from, to: destination.to, insert }];
  });
}

export async function imagePathRewriteEditsAsync(
  current: string,
  rewrites: ReadonlyArray<{ source: string; destination: string }>,
  checkpoint: WorkCheckpoint,
): Promise<TextEdit[]> {
  return imagePathRewriteEdits(current, rewrites, await parseImageDestinations(current, checkpoint));
}

export function literalReplacementEdits(current: string, before: string, after: string): TextEdit[] {
  if (!before) return [];
  const edits: TextEdit[] = [];
  for (let from = current.indexOf(before); from >= 0; from = current.indexOf(before, from + before.length)) {
    edits.push({ from, to: from + before.length, insert: after });
  }
  return edits;
}

export function completedUploadEdits(current: string, placeholder: string, replacement: string, destinations?: ImageDestination[]): TextEdit[] {
  // Keep exact replacements for the original label (including malformed file
  // names), but resolve the upload URL independently if its label was edited.
  const edits = literalReplacementEdits(current, placeholder, replacement);
  const upload = /\((inkflow-upload:\/\/[^)\s]+)\)$/.exec(placeholder)?.[1];
  if (!upload) return edits;
  const destination = replacement ? collectImageDestinations(replacement)[0]?.destination ?? "" : "";
  const candidates = [
    ...(replacement ? imagePathRewriteEdits(current, [{ source: upload, destination }], destinations) : []),
    ...literalReplacementEdits(current, upload, destination.replace(/\s/g, character =>
      `%${character.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0")}`)),
  ];
  for (const edit of candidates) {
    if (!edits.some(existing => edit.from < existing.to && edit.to > existing.from)) edits.push(edit);
  }
  return edits.sort((left, right) => left.from - right.from);
}

export async function completedUploadEditsAsync(
  current: string, placeholder: string, replacement: string, checkpoint: WorkCheckpoint,
): Promise<TextEdit[]> {
  const upload = /\((inkflow-upload:\/\/[^)\s]+)\)$/.exec(placeholder)?.[1];
  const destinations = replacement && upload && current.includes(upload)
    ? await parseImageDestinations(current, checkpoint) : [];
  return completedUploadEdits(current, placeholder, replacement, destinations);
}

function imageRewriteMap(saved: string, rewritten: string): Map<string, string | null> {
  const rewrites = new Map<string, string | null>();
  const before = collectImageDestinations(saved);
  const after = collectImageDestinations(rewritten);
  if (before.length !== after.length) return rewrites;
  before.forEach((destination, index) => {
    const next = after[index];
    if (
      destination.syntax === next.syntax
      && destination.raw !== next.raw
    ) {
      // Only generated targets are percent-decoded, exactly once. Source URLs
      // keep their percent spelling: legacy literal-% filenames can otherwise
      // be confused with a different, percent-decoded source asset.
      let target: string;
      try { target = decodeURIComponent(next.destination); }
      catch { return; }
      const key = destination.destination;
      const existing = rewrites.get(key);
      // Quoted/unquoted occurrences may encode the same target differently.
      // If they really disagree about the target, do not guess a replacement.
      rewrites.set(key, existing === undefined || existing === target ? target : null);
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
      const target = rewrites.get(destination.destination);
      if (target === undefined || target === null) return [];
      const replacement = encodeImageDestinationPath(target, destination);
      return replacement !== destination.raw
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
  savedVersion: number,
  currentContent: string,
): { tab: DocumentTab; needsResave: boolean } {
  const changedDuringSave = tab.editorVersion !== savedVersion;
  const content = result.content
    ? changedDuringSave
      ? mergeImageRewrites(currentContent, savedContent, result.content)
      : result.content
    : currentContent;
  const contentChanged = content !== currentContent;
  return {
    tab: {
      ...tab,
      path: result.path,
      readOnly: false,
      content: contentChanged ? textFromString(content) : tab.content,
      editorVersion: contentChanged ? tab.editorVersion + 1 : tab.editorVersion,
      revision: result.revision,
      dirty: changedDuringSave,
      saveState: changedDuringSave ? "dirty" : "saved",
      externalChange: null,
    },
    needsResave: changedDuringSave,
  };
}

export function textFromString(content: string): Text {
  return Text.of(content.split("\n"));
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
