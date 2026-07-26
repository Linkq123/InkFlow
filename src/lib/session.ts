import type { DocumentTab, SessionTabV1, SessionV1 } from "./api/types";

export function buildSessionSnapshot(
  workspaceRoot: string | null,
  tabs: readonly DocumentTab[],
  activeId: string,
): SessionV1 {
  const sessionTabs: SessionTabV1[] = tabs.flatMap((tab) => tab.path
    ? [{ path: tab.path, mode: tab.mode }]
    : []);
  const active = tabs.find((tab) => tab.id === activeId);
  return {
    schemaVersion: 1,
    workspaceRoot,
    tabs: sessionTabs,
    activePath: active?.path ?? sessionTabs[0]?.path ?? null,
  };
}

export function activeFirstSessionTabs(session: SessionV1): SessionTabV1[] {
  if (!session.activePath) return [...session.tabs];
  const activePath = documentPathKey(session.activePath);
  const active = session.tabs.find((tab) => documentPathKey(tab.path) === activePath);
  return active
    ? [active, ...session.tabs.filter((tab) => tab !== active)]
    : [...session.tabs];
}

export interface RestoredDocumentPartition {
  additions: DocumentTab[];
  matchedExisting: DocumentTab[];
  redundant: DocumentTab[];
}

export function partitionRestoredDocuments(
  currentTabs: readonly DocumentTab[],
  incoming: readonly DocumentTab[],
): RestoredDocumentPartition {
  const currentIds = new Set(currentTabs.map((tab) => tab.id));
  const byPath = new Map<string, DocumentTab>();
  for (const tab of currentTabs) {
    if (tab.path) byPath.set(documentPathKey(tab.path), tab);
  }

  const additions: DocumentTab[] = [];
  const matchedExisting: DocumentTab[] = [];
  const redundant: DocumentTab[] = [];
  const matchedIds = new Set<string>();
  for (const document of incoming) {
    const key = document.path ? documentPathKey(document.path) : null;
    const existing = key ? byPath.get(key) : undefined;
    if (existing) {
      redundant.push(document);
      if (currentIds.has(existing.id) && !matchedIds.has(existing.id)) {
        matchedExisting.push(existing);
        matchedIds.add(existing.id);
      }
      continue;
    }
    additions.push(document);
    if (key) byPath.set(key, document);
  }
  return { additions, matchedExisting, redundant };
}

export function isPristineStartupPlaceholder(
  startupPlaceholderId: string | null,
  tabs: readonly DocumentTab[],
): boolean {
  if (!startupPlaceholderId || tabs.length !== 1) return false;
  const [tab] = tabs;
  return tab.id === startupPlaceholderId
    && tab.path === null
    && !tab.dirty
    && tab.content.length === 0;
}

export function orderRestoredSessionTabs(
  session: SessionV1,
  currentTabs: readonly DocumentTab[],
  restoredIds: ReadonlySet<string>,
): DocumentTab[] {
  const sessionOrder = new Map(
    session.tabs.map((tab, index) => [documentPathKey(tab.path), index]),
  );
  const restored = currentTabs
    .filter((tab) => restoredIds.has(tab.id))
    .sort((left, right) =>
      (sessionOrder.get(documentPathKey(left.path ?? "")) ?? 0)
      - (sessionOrder.get(documentPathKey(right.path ?? "")) ?? 0));
  const extras = currentTabs.filter((tab) => !restoredIds.has(tab.id));
  return [...restored, ...extras];
}

export function uniqueDocumentPaths(
  paths: readonly string[],
  excludedKeys: ReadonlySet<string> = new Set(),
): string[] {
  const seen = new Set(excludedKeys);
  return paths.filter((path) => {
    const key = documentPathKey(path);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

export function documentPathKey(path: string): string {
  return path.replace(/\//g, "\\").toLowerCase();
}
