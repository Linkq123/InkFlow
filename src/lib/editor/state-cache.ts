import {
  history,
  historyField,
  isolateHistory,
  redo,
  undo,
} from "@codemirror/commands";
import {
  ChangeSet,
  EditorSelection,
  EditorState,
  MapMode,
  Text,
  Transaction,
  type Extension,
  type StateCommand,
} from "@codemirror/state";
import type { TextEdit } from "../document-state";
import { cooperativeWork, type WorkCheckpoint } from "../async";

export interface CachedEditorStateV2 {
  version: 2;
  documentVersion: number;
  selection: EditorSelection;
  history: unknown;
}

export interface EditorHistoryRewrite {
  previousDoc: Text;
  nextDoc: Text;
  documentVersion: number;
  edits: TextEdit[];
}

/** Visit both branches without changing the live editor or retaining document copies. */
export function* editorHistoryDocuments(state: EditorState): Generator<Text> {
  const initial = detachedHistoryState(state);
  yield initial.doc;
  for (const command of [undo, redo]) {
    let cursor = initial;
    for (;;) {
      const step = runHistoryCommand(cursor, command);
      if (!step) break;
      yield step.state.doc;
      cursor = step.state;
    }
  }
}

export async function transformEditorHistoryAsync(
  state: EditorState,
  extensions: Extension[],
  transform: (doc: Text) => Promise<readonly TextEdit[]>,
  checkpoint: WorkCheckpoint = cooperativeWork(),
): Promise<EditorState> {
  const current = createRebasedSnapshot(state, await transform(state.doc));
  const initial = detachedHistoryState(state);
  const past = [current];
  const future: RebasedSnapshot[] = [];
  for (const command of [undo, redo]) {
    let cursor = initial;
    let snapshot = current;
    for (;;) {
      const pause = checkpoint();
      if (pause) await pause;
      const step = runHistoryCommand(cursor, command);
      if (!step) break;
      const next = createRebasedSnapshot(step.state, await transform(step.state.doc));
      if (command === undo) {
        snapshot.forwardFromPrevious = rebaseTransition(next, snapshot, step.transaction.changes.invert(cursor.doc));
        past.push(next);
      } else {
        next.forwardFromPrevious = rebaseTransition(snapshot, next, step.transaction.changes);
        future.push(next);
      }
      cursor = step.state;
      snapshot = next;
    }
  }
  past.reverse();
  let rebuilt = EditorState.create({ doc: past[0].rewrittenDoc, selection: past[0].selection, extensions: [history()] });
  let futureEvents = 0;
  for (const branch of [past.slice(1), future]) {
    for (const snapshot of branch) {
      const pause = checkpoint();
      if (pause) await pause;
      if (branch === future && snapshot.forwardFromPrevious?.empty === false) futureEvents++;
      rebuilt = appendHistorySnapshot(rebuilt, snapshot);
    }
  }
  for (let index = 0; index < futureEvents; index++) {
    const pause = checkpoint();
    if (pause) await pause;
    const previous = runHistoryCommand(rebuilt, undo);
    if (!previous) break;
    rebuilt = previous.state;
  }
  if (!rebuilt.doc.eq(current.rewrittenDoc)) throw new Error("Rebased history diverged from the current document.");
  return EditorState.create({
    doc: current.rewrittenDoc, selection: current.selection,
    extensions: withHistoryValue(extensions, rebuilt.field(historyField, false)),
  });
}

function detachedHistoryState(state: EditorState): EditorState {
  return EditorState.create({
    doc: state.doc,
    selection: state.selection,
    extensions: withHistoryValue([history()], state.field(historyField, false)),
  });
}

interface TrackedEdit extends TextEdit {
  original: string;
}

interface RebasedSnapshot {
  originalDoc: Text;
  rewrittenDoc: Text;
  selection: EditorSelection;
  systemChanges: ChangeSet;
  forwardFromPrevious?: ChangeSet;
}

interface HistoryStep {
  state: EditorState;
  transaction: Transaction;
}

export function createCachedEditorState(
  doc: string | Text,
  extensions: Extension[],
  cached: unknown,
  documentVersion: number,
): EditorState {
  if (isCachedEditorState(cached) && cached.documentVersion === documentVersion) {
    try {
      return EditorState.create({
        doc,
        selection: cached.selection,
        extensions: withHistoryValue(extensions, cached.history),
      });
    } catch {
      // A stale or incompatible cache must never prevent the document from opening.
    }
  }
  return EditorState.create({ doc, extensions });
}

export function cacheEditorState(
  state: EditorState,
  documentVersion: number,
): CachedEditorStateV2 {
  return {
    version: 2,
    documentVersion,
    selection: state.selection,
    history: state.field(historyField, false),
  };
}

export function rebaseEditorState(
  state: EditorState,
  nextDoc: string | Text,
  extensions: Extension[],
  edits: readonly TextEdit[],
): EditorState {
  try {
    const nextText = asText(nextDoc);
    const tracked = trackEdits(state.doc, edits);
    const current = createRebasedSnapshot(state, tracked);
    if (!current.rewrittenDoc.eq(nextText)) {
      return resetEditorState(state, nextText, extensions);
    }

    // Replay in a detached state so the live editor's read-only setting and
    // transaction filters cannot suppress system rewrites. The document and
    // history are immutable and can be shared without copying their contents.
    const historyState = detachedHistoryState(state);
    // Walk the public undo/redo commands and retain CodeMirror's persistent Text
    // trees, not full-document strings. This rewrites historical asset paths
    // without multiplying a large document by the history depth.
    const past = collectPastSnapshots(historyState, current, tracked);
    const future = collectFutureSnapshots(historyState, current, tracked);
    let rebuilt = EditorState.create({
      doc: past[0].rewrittenDoc,
      selection: past[0].selection,
      extensions: [history()],
    });
    for (const snapshot of past.slice(1)) {
      rebuilt = appendHistorySnapshot(rebuilt, snapshot);
    }

    let futureEvents = 0;
    for (const snapshot of future) {
      if (snapshot.forwardFromPrevious?.empty === false) futureEvents += 1;
      rebuilt = appendHistorySnapshot(rebuilt, snapshot);
    }
    for (let index = 0; index < futureEvents; index += 1) {
      const previous = runHistoryCommand(rebuilt, undo);
      if (!previous) break;
      rebuilt = previous.state;
    }

    if (!rebuilt.doc.eq(current.rewrittenDoc)) {
      return resetEditorState(state, nextDoc, extensions);
    }
    return EditorState.create({
      doc: current.rewrittenDoc,
      selection: current.selection,
      extensions: withHistoryValue(
        extensions,
        rebuilt.field(historyField, false),
      ),
    });
  } catch {
    return resetEditorState(state, nextDoc, extensions);
  }
}

export function rebaseCachedEditorState(
  cached: unknown,
  previousDoc: string,
  previousVersion: number,
  nextDoc: string,
  nextVersion: number,
  edits: readonly TextEdit[],
): CachedEditorStateV2 {
  const previous = createCachedEditorState(
    previousDoc,
    [history()],
    cached,
    previousVersion,
  );
  const rebased = rebaseEditorState(previous, nextDoc, [history()], edits);
  return cacheEditorState(rebased, nextVersion);
}

function collectPastSnapshots(
  initialState: EditorState,
  initialSnapshot: RebasedSnapshot,
  initialEdits: TrackedEdit[],
): RebasedSnapshot[] {
  const snapshots = [initialSnapshot];
  let state = initialState;
  let snapshot = initialSnapshot;
  let edits = initialEdits;
  for (;;) {
    const step = runHistoryCommand(state, undo);
    if (!step) break;
    const previousEdits = mapTrackedEdits(edits, step.transaction.changes, step.state.doc);
    const previous = createRebasedSnapshot(step.state, previousEdits);
    const originalForward = step.transaction.changes.invert(state.doc);
    snapshot.forwardFromPrevious = rebaseTransition(
      previous,
      snapshot,
      originalForward,
    );
    snapshots.push(previous);
    state = step.state;
    snapshot = previous;
    edits = previousEdits;
  }
  return snapshots.reverse();
}

function collectFutureSnapshots(
  initialState: EditorState,
  initialSnapshot: RebasedSnapshot,
  initialEdits: TrackedEdit[],
): RebasedSnapshot[] {
  const snapshots: RebasedSnapshot[] = [];
  let state = initialState;
  let snapshot = initialSnapshot;
  let edits = initialEdits;
  for (;;) {
    const step = runHistoryCommand(state, redo);
    if (!step) break;
    const nextEdits = mapTrackedEdits(edits, step.transaction.changes, step.state.doc);
    const next = createRebasedSnapshot(step.state, nextEdits);
    next.forwardFromPrevious = rebaseTransition(
      snapshot,
      next,
      step.transaction.changes,
    );
    snapshots.push(next);
    state = step.state;
    snapshot = next;
    edits = nextEdits;
  }
  return snapshots;
}

function createRebasedSnapshot(
  state: EditorState,
  edits: readonly TextEdit[],
): RebasedSnapshot {
  const systemChanges = ChangeSet.of(edits, state.doc.length);
  return {
    originalDoc: state.doc,
    rewrittenDoc: systemChanges.apply(state.doc),
    selection: state.selection.map(systemChanges.desc),
    systemChanges,
  };
}

function rebaseTransition(
  from: RebasedSnapshot,
  to: RebasedSnapshot,
  originalChanges: ChangeSet,
): ChangeSet {
  // A' -> A (remove the system rewrite), A -> B (the user edit),
  // then B -> B' (apply the rewrite appropriate for the next snapshot).
  return from.systemChanges
    .invert(from.originalDoc)
    .compose(originalChanges)
    .compose(to.systemChanges);
}

function appendHistorySnapshot(
  state: EditorState,
  snapshot: RebasedSnapshot,
): EditorState {
  const changes = snapshot.forwardFromPrevious;
  if (!changes || changes.empty) {
    return state.update({
      selection: snapshot.selection,
      annotations: Transaction.addToHistory.of(false),
    }).state;
  }
  const next = state.update({
    changes,
    selection: snapshot.selection,
    annotations: isolateHistory.of("full"),
  }).state;
  if (!next.doc.eq(snapshot.rewrittenDoc)) {
    throw new Error("Rebased history diverged from the expected document.");
  }
  return next;
}

function trackEdits(doc: Text, edits: readonly TextEdit[]): TrackedEdit[] {
  return edits.map((edit) => {
    if (edit.from < 0 || edit.to < edit.from || edit.to > doc.length) {
      throw new RangeError("Editor history rewrite is outside the document.");
    }
    return {
      ...edit,
      original: doc.sliceString(edit.from, edit.to),
    };
  });
}

function mapTrackedEdits(
  edits: readonly TrackedEdit[],
  changes: ChangeSet,
  nextDoc: Text,
): TrackedEdit[] {
  const mapped: TrackedEdit[] = [];
  for (const edit of edits) {
    const from = changes.mapPos(edit.from, 1, MapMode.TrackDel);
    const to = changes.mapPos(edit.to, -1, MapMode.TrackDel);
    if (
      from === null
      || to === null
      || to < from
      || nextDoc.sliceString(from, to) !== edit.original
    ) {
      continue;
    }
    mapped.push({ ...edit, from, to });
  }
  return mapped;
}

function runHistoryCommand(
  state: EditorState,
  command: StateCommand,
): HistoryStep | null {
  const dispatched: { transaction?: Transaction } = {};
  const applied = command({
    state,
    dispatch: (next) => {
      dispatched.transaction = next;
    },
  });
  const transaction = dispatched.transaction;
  return applied && transaction
    ? { state: transaction.state, transaction }
    : null;
}

function resetEditorState(
  state: EditorState,
  nextDoc: string | Text,
  extensions: Extension[],
): EditorState {
  const nextText = asText(nextDoc);
  return EditorState.create({
    doc: nextText,
    selection: state.selection.map(
      changesBetween(state.doc.toString(), nextText.toString()).desc,
    ),
    extensions,
  });
}

function asText(value: string | Text): Text {
  return typeof value === "string" ? Text.of(value.split("\n")) : value;
}

function changesBetween(before: string, after: string): ChangeSet {
  if (before === after) return ChangeSet.empty(before.length);
  let from = 0;
  const shared = Math.min(before.length, after.length);
  while (from < shared && before.charCodeAt(from) === after.charCodeAt(from)) {
    from += 1;
  }
  let beforeTo = before.length;
  let afterTo = after.length;
  while (
    beforeTo > from
    && afterTo > from
    && before.charCodeAt(beforeTo - 1) === after.charCodeAt(afterTo - 1)
  ) {
    beforeTo -= 1;
    afterTo -= 1;
  }
  return ChangeSet.of({
    from,
    to: beforeTo,
    insert: after.slice(from, afterTo),
  }, before.length);
}

function withHistoryValue(extensions: Extension[], value: unknown): Extension[] {
  return value === undefined
    ? extensions
    : [extensions, historyField.init(() => value)];
}

function isCachedEditorState(value: unknown): value is CachedEditorStateV2 {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Partial<CachedEditorStateV2>;
  return candidate.version === 2
    && Number.isSafeInteger(candidate.documentVersion)
    && candidate.selection instanceof EditorSelection;
}
