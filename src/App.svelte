<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { Text } from "@codemirror/state";
  import { confirm, open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
  import {
    AlignLeft,
    BookOpen,
    Braces,
    Check,
    ChevronDown,
    FileDown,
    FilePlus2,
    Files,
    Focus,
    FolderOpen,
    History,
    ListTree,
    Menu,
    MoveVertical,
    MoreHorizontal,
    PanelLeftClose,
    PanelLeftOpen,
    PanelRightClose,
    PanelRightOpen,
    Save,
    Search,
    Settings,
    X,
  } from "@lucide/svelte";
  import "katex/dist/katex.min.css";
  import { api, isDesktop, messageFromError } from "./lib/api/client";
  import type {
    DocumentSnapshot,
    DocumentTab,
    ExportRequest,
    RecoveryEntry,
    SaveDocumentRequest,
    SaveOutcome,
    SearchHit,
    SessionV1,
    SettingsV1,
    WorkspaceEntry,
    WorkspaceSnapshot,
  } from "./lib/api/types";
  import CommandPalette from "./lib/components/CommandPalette.svelte";
  import ConflictDialog from "./lib/components/ConflictDialog.svelte";
  import type { PaletteCommand } from "./lib/components/palette";
  import FileSidebar from "./lib/components/FileSidebar.svelte";
  import MarkdownEditor from "./lib/components/MarkdownEditor.svelte";
  import MarkdownPreview from "./lib/components/MarkdownPreview.svelte";
  import OutlineSidebar from "./lib/components/OutlineSidebar.svelte";
  import RecoveryDialog from "./lib/components/RecoveryDialog.svelte";
  import SettingsDialog from "./lib/components/SettingsDialog.svelte";
  import WorkspaceSearch from "./lib/components/WorkspaceSearch.svelte";
  import { resolveLocale, translate } from "./lib/i18n";
  import {
    analyzeMarkdownInWorker,
    renderInWorker,
    type MarkdownAnalysis,
  } from "./lib/markdown/render-service";
  import {
    blockRemoteImageRequests,
    hasRemoteMermaidImageReference,
    isRemoteImageSource,
  } from "./lib/markdown/resources";
  import { waitForPromiseOrTimeout } from "./lib/async";
  import { DocumentSerializer } from "./lib/document-buffer";
  import {
    applyTextEdits,
    applySavedResult,
    imageRewriteEditsBetween,
    isPathAffected,
    relocatedPath,
    replaceUploadPlaceholder,
    uploadPlaceholderEdit,
    textFromString,
    type TextEdit,
    withoutTabsById,
  } from "./lib/document-state";
  import {
    rebaseCachedEditorState,
    type EditorHistoryRewrite,
  } from "./lib/editor/state-cache";
  import { createLatestSerializedWriter } from "./lib/latest-serialized-writer";
  import {
    activeFirstSessionTabs,
    buildSessionSnapshot,
    documentPathKey,
    isPristineStartupPlaceholder,
    orderRestoredSessionTabs,
    partitionRestoredDocuments,
    uniqueDocumentPaths,
  } from "./lib/session";
  import { markInteractive } from "./lib/performance";
  import {
    createDeferredHydration,
    type ValueMutation,
  } from "./lib/settings-hydration";
  import { documentStats, type DocumentStats, type OutlineItem } from "./lib/stats";

  const defaultSettings: SettingsV1 = {
    schemaVersion: 1,
    locale: "system",
    theme: "system",
    pageWidth: 820,
    fontSize: 16,
    lineHeight: 1.75,
    editorFont: "Segoe UI Variable, Microsoft YaHei UI, sans-serif",
    codeFont: "Cascadia Mono, Consolas, monospace",
    autosaveDelayMs: 750,
    showFileTree: false,
    showOutline: false,
    focusMode: false,
    typewriterMode: false,
    recentFiles: [],
    recentWorkspaces: [],
  };
  const MAX_IMAGE_BYTES = 50 * 1024 * 1024;
  const SESSION_WRITE_RETRIES = 3;
  const emptySession: SessionV1 = {
    schemaVersion: 1,
    workspaceRoot: null,
    tabs: [],
    activePath: null,
  };
  const settingsHydration = createDeferredHydration<SettingsV1>(!isDesktop());

  let settings = structuredClone(defaultSettings);
  let tabs: DocumentTab[] = [newUntitled()];
  let activeId = tabs[0].id;
  let workspace: WorkspaceSnapshot | null = null;
  let editor: MarkdownEditor | null = null;
  let preview: MarkdownPreview | null = null;
  let paletteOpen = false;
  let quickOpen = false;
  let overflowOpen = false;
  let tabListOpen = false;
  let menuReturnFocus: HTMLElement | null = null;
  let tabListReturnFocus: HTMLElement | null = null;
  let searchOpen = false;
  let searchResults: SearchHit[] = [];
  let searchResultQuery = "";
  let searching = false;
  let searchRevision = 0;
  let settingsOpen = false;
  let recoveryOpen = false;
  let recoveryEntries: RecoveryEntry[] = [];
  let recoveryLoading = false;
  let conflictDisk: DocumentSnapshot | null = null;
  let conflictDocumentId: string | null = null;
  let toast = "";
  let toastKind: "info" | "error" = "info";
  let toastPersistent = false;
  let toastAction: (() => void) | null = null;
  let toastActionLabel = "";
  let toastRevision = 0;
  let welcomeDismissed = new Set<string>();
  let printHtml = "";
  let saveTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let checkpointTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let checkpointMaxTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let saveQueues = new Map<string, Promise<boolean>>();
  const settingsWriter = createLatestSerializedWriter<SettingsV1>(
    (snapshot) => api.updateSettings(snapshot),
    (normalized) => settings = normalized,
  );
  const sessionWriter = createLatestSerializedWriter<SessionV1>(
    (snapshot) => api.updateSession(snapshot),
    () => undefined,
  );
  let sessionReady = false;
  let queuedOpenPaths: string[] = [];
  let sessionTimer: ReturnType<typeof setTimeout> | null = null;
  let lastSessionKey = "";
  let pendingSessionKey: string | null = null;
  let workspaceRequestRevision = 0;
  let workspaceOpenTail: Promise<void> = Promise.resolve();
  let editorStates = new Map<string, unknown>();
  let pendingEditorRewrites = new Map<string, EditorHistoryRewrite>();
  let interactionLockedTabs = new Set<string>();
  let suspendedSaves = new Set<string>();
  let externalTimer: ReturnType<typeof setInterval> | null = null;
  let externalPollRunning = false;
  let analysisTimer: ReturnType<typeof setTimeout> | null = null;
  let analysisRevision = 0;
  let displayedAnalysisDocumentId: string | null = null;
  let stats: DocumentStats = documentStats("");
  let outline: OutlineItem[] = [];
  let analysisCache = new Map<string, { version: number; result: MarkdownAnalysis }>();
  const documentSerializer = new DocumentSerializer();
  let hasBlockedRemoteImages = false;
  let unlistenPaths: UnlistenFn | null = null;
  let unlistenClose: UnlistenFn | null = null;
  let closing = false;
  let interactiveMarked = false;

  $: active = tabs.find((tab) => tab.id === activeId) ?? tabs[0];
  $: conflictTab = conflictDocumentId ? tabs.find((tab) => tab.id === conflictDocumentId) ?? null : null;
  $: scheduleDocumentAnalysis(
    active?.id ?? null,
    active?.editorVersion ?? -1,
    active?.allowRemoteImages ?? false,
  );
  $: locale = resolveLocale(settings.locale);
  $: effectiveTheme = resolveTheme(settings.theme);
  $: t = (key: Parameters<typeof translate>[1], values: Record<string, string | number> = {}) => translate(locale, key, values);
  $: commands = buildCommands();
  $: quickOpenCommands = buildQuickOpenCommands();
  $: scheduleSessionPersistence(buildSessionSnapshot(workspace?.root ?? null, tabs, activeId));
  $: showWelcome = !!active
    && !active.path
    && active.content.length === 0
    && !workspace
    && !welcomeDismissed.has(active.id);
  $: revealActiveTab(activeId);
  $: if (typeof document !== "undefined") {
    document.documentElement.dataset.theme = effectiveTheme;
    document.documentElement.lang = locale;
  }
  $: if (isDesktop() && active) {
    void getCurrentWindow().setTitle(`${active.dirty ? "● " : ""}${active.title} — InkFlow`).catch(() => undefined);
  }

  onMount(() => {
    void initialize();
    const keyHandler = (event: KeyboardEvent) => handleGlobalKey(event);
    const pointerHandler = (event: MouseEvent) => {
      const target = event.target instanceof Element ? event.target : null;
      if (!target?.closest(".app-menu,[data-app-menu-trigger]")) overflowOpen = false;
      if (!target?.closest(".tab-list-menu,[data-tab-list-trigger]")) tabListOpen = false;
    };
    window.addEventListener("keydown", keyHandler);
    window.addEventListener("mousedown", pointerHandler);
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const themeHandler = () => settings = { ...settings };
    media.addEventListener("change", themeHandler);
    externalTimer = setInterval(() => void pollExternalChanges(), 2200);
    return () => {
      window.removeEventListener("keydown", keyHandler);
      window.removeEventListener("mousedown", pointerHandler);
      media.removeEventListener("change", themeHandler);
      if (externalTimer) clearInterval(externalTimer);
      if (analysisTimer) clearTimeout(analysisTimer);
      if (sessionTimer) clearTimeout(sessionTimer);
      unlistenPaths?.();
      unlistenClose?.();
      for (const timer of saveTimers.values()) clearTimeout(timer);
      for (const timer of checkpointTimers.values()) clearTimeout(timer);
      for (const timer of checkpointMaxTimers.values()) clearTimeout(timer);
    };
  });

  async function initialize(): Promise<void> {
    if (!isDesktop()) return;
    try {
      const appWindow = getCurrentWindow();
      [unlistenPaths, unlistenClose] = await Promise.all([
        listen<string[]>("app-open-paths", (event) => {
          if (sessionReady) void openPaths(event.payload);
          else queuedOpenPaths.push(...event.payload);
        }),
        appWindow.onCloseRequested(async (event) => {
          if (closing) return;
          event.preventDefault();
          try {
            await confirmCloseWindow();
          } catch (error) {
            showToast(messageFromError(error), "error");
          }
        }),
      ]);
      const [, startupPaths, session] = await Promise.all([
        initializeSettings(),
        api.takeStartupPaths(),
        loadSessionForStartup(),
      ]);
      const requestedPaths = [...startupPaths, ...queuedOpenPaths];
      queuedOpenPaths = [];
      if (requestedPaths.length) await openPaths(requestedPaths);
      else await restoreSession(session);
      lastSessionKey = sessionKey(session);
      await drainQueuedOpenPaths();
      sessionReady = true;
      scheduleSessionPersistence(buildSession());
      markAppInteractive();
      scheduleRecoveryCheck();
    } catch (error) {
      await drainQueuedOpenPaths();
      sessionReady = true;
      markAppInteractive();
      showToast(messageFromError(error), "error");
    }
  }

  async function loadSessionForStartup(): Promise<SessionV1> {
    try {
      return await api.getSession();
    } catch {
      showToast(t("sessionCorrupt"));
      return structuredClone(emptySession);
    }
  }

  async function drainQueuedOpenPaths(): Promise<void> {
    while (queuedOpenPaths.length) {
      const paths = queuedOpenPaths;
      queuedOpenPaths = [];
      await openPaths(paths);
    }
  }

  function scheduleRecoveryCheck(): void {
    const check = () => void api.listRecovery().then(
      (entries) => {
        if (entries.some((entry) => entry.kind === "draft")) showToast(t("recoverDetected"));
      },
      () => undefined,
    );
    if ("requestIdleCallback" in window) {
      window.requestIdleCallback(check, { timeout: 2_000 });
    } else {
      setTimeout(check, 600);
    }
  }

  async function restoreSession(session: SessionV1): Promise<void> {
    let skipped = 0;
    const workspaceRestoreRevision = workspaceRequestRevision;
    const ordered = activeFirstSessionTabs(session);
    const restored: DocumentTab[] = [];
    let restoredActivated = false;
    const startupPlaceholderId = tabs.length === 1 && tabs[0].path === null
      ? tabs[0].id
      : null;

    const publish = (documents: DocumentTab[]): void => {
      const { additions, matchedExisting, redundant } = partitionRestoredDocuments(tabs, documents);
      for (const duplicate of redundant) {
        if (!tabs.some((tab) => tab.id === duplicate.id)) {
          void api.closeDocument(duplicate.id).catch(() => undefined);
        }
      }
      const restoredIds = new Set(restored.map((tab) => tab.id));
      restored.push(
        ...matchedExisting.filter((tab) => !restoredIds.has(tab.id)),
        ...additions,
      );
      if (!additions.length) return;
      if (!restoredActivated) {
        if (isPristineStartupPlaceholder(startupPlaceholderId, tabs)) {
          tabs = [...additions];
          activeId = additions.find((tab) => tab.path === session.activePath)?.id ?? additions[0].id;
        } else {
          const currentIds = new Set(tabs.map((tab) => tab.id));
          tabs = [...tabs, ...additions.filter((tab) => !currentIds.has(tab.id))];
        }
        restoredActivated = true;
        markAppInteractive();
      } else {
        const currentIds = new Set(tabs.map((tab) => tab.id));
        tabs = [...tabs, ...additions.filter((tab) => !currentIds.has(tab.id))];
      }
    };

    const activeSessionTab = ordered[0];
    if (activeSessionTab) {
      const result = await Promise.allSettled([api.openPaths([activeSessionTab.path], false)]);
      const opened = result[0];
      if (opened.status === "fulfilled" && opened.value.length) {
        publish(opened.value.map((snapshot) => ({ ...fromSnapshot(snapshot), mode: activeSessionTab.mode })));
      } else {
        skipped += 1;
      }
    }
    markAppInteractive();

    if (session.workspaceRoot) {
      try {
        const restoredWorkspace = await openWorkspaceSerialized(
          session.workspaceRoot,
          false,
          workspaceRestoreRevision,
        );
        if (restoredWorkspace) workspace = restoredWorkspace;
      } catch {
        skipped += 1;
      }
    }

    const remaining = ordered.slice(activeSessionTab ? 1 : 0);
    for (let index = 0; index < remaining.length; index += 2) {
      const chunk = remaining.slice(index, index + 2);
      const results = await Promise.allSettled(
        chunk.map((tab) => api.openPaths([tab.path], false)),
      );
      const additions: DocumentTab[] = [];
      results.forEach((result, resultIndex) => {
        if (result.status === "rejected" || result.value.length === 0) {
          skipped += 1;
          return;
        }
        const mode = chunk[resultIndex].mode;
        additions.push(...result.value.map((snapshot) => ({ ...fromSnapshot(snapshot), mode })));
      });
      publish(additions);
      if (index + 2 < remaining.length) await yieldToBrowser();
    }
    if (restored.length) {
      const restoredIds = new Set(restored.map((tab) => tab.id));
      tabs = orderRestoredSessionTabs(session, tabs, restoredIds);
      if (!tabs.some((tab) => tab.id === activeId)) {
        activeId = restored.find((tab) => tab.path === session.activePath)?.id ?? restored[0].id;
      }
      revealActiveTab(activeId);
    }
    if (skipped) showToast(t("sessionSkipped", { count: skipped }));
  }

  function yieldToBrowser(): Promise<void> {
    return new Promise((resolve) => requestAnimationFrame(() => resolve()));
  }

  function markAppInteractive(): void {
    if (interactiveMarked) return;
    interactiveMarked = true;
    void tick().then(() => {
      markInteractive();
      if (isDesktop()) void api.markPerformanceReady().catch(() => undefined);
    });
  }

  async function initializeSettings(): Promise<void> {
    try {
      const loadedSettings = await api.getSettings();
      const hydrated = settingsHydration.hydrate(loadedSettings);
      settings = hydrated.value;
      if (hydrated.shouldPersist) await persistSettings();
    } catch (error) {
      const fallback = settingsHydration.completeWithCurrent(settings);
      settings = fallback.value;
      if (fallback.shouldPersist) await persistSettings();
      showToast(messageFromError(error), "error");
    }
  }

  function scheduleDocumentAnalysis(
    documentId: string | null,
    documentVersion: number,
    allowRemoteImages: boolean,
  ): void {
    const documentChanged = displayedAnalysisDocumentId !== documentId;
    displayedAnalysisDocumentId = documentId;
    const revision = ++analysisRevision;
    if (analysisTimer) clearTimeout(analysisTimer);
    analysisTimer = null;
    if (!documentId) {
      stats = documentStats("");
      outline = [];
      hasBlockedRemoteImages = false;
      return;
    }
    const cached = analysisCache.get(documentId);
    if (cached?.version === documentVersion) {
      stats = cached.result.stats;
      outline = cached.result.outline;
      hasBlockedRemoteImages = !allowRemoteImages && cached.result.hasRemoteImages;
      return;
    }
    const tab = tabs.find((item) => item.id === documentId);
    if (tab) {
      stats = documentChanged
        ? { words: 0, lines: tab.content.lines, characters: tab.content.length }
        : { ...stats, lines: tab.content.lines, characters: tab.content.length };
    }
    if (documentChanged) outline = [];
    hasBlockedRemoteImages = false;
    analysisTimer = setTimeout(() => {
      analysisTimer = null;
      const requested = tabs.find((item) => item.id === documentId);
      if (!requested || requested.editorVersion !== documentVersion) return;
      const markdown = serializeTab(requested);
      void analyzeMarkdownInWorker(markdown).then(
        (result) => {
          const current = tabs.find((tab) => tab.id === documentId);
          if (
            revision === analysisRevision
            && activeId === documentId
            && current
            && current.editorVersion === documentVersion
          ) {
            analysisCache.set(documentId, { version: documentVersion, result });
            stats = result.stats;
            outline = result.outline;
            hasBlockedRemoteImages = !current.allowRemoteImages && result.hasRemoteImages;
          }
        },
        // Analysis is advisory. Rendering still strips remote request attributes.
        () => undefined,
      );
    }, 300);
  }

  function newUntitled(content = "", title?: string): DocumentTab {
    return {
      id: crypto.randomUUID(),
      path: null,
      title: title ?? translate(resolveLocale(settings.locale), "untitled"),
      content: textFromString(content),
      encoding: "utf-8",
      eol: "lf",
      hadBom: false,
      hadFinalNewline: false,
      readOnly: false,
      revision: null,
      dirty: content.length > 0,
      saveState: content.length > 0 ? "dirty" : "saved",
      mode: "live",
      externalChange: null,
      allowRemoteImages: false,
      editorVersion: 0,
    };
  }

  function fromSnapshot(snapshot: DocumentSnapshot): DocumentTab {
    return {
      ...snapshot,
      content: textFromString(snapshot.content),
      dirty: false,
      saveState: "saved",
      mode: "live",
      externalChange: null,
      allowRemoteImages: false,
      editorVersion: 0,
    };
  }

  function createDocument(): void {
    const blank = tabs.length === 1 && !tabs[0].path && tabs[0].content.length === 0 && !tabs[0].dirty;
    if (blank) {
      activeId = tabs[0].id;
      editor?.focus();
      return;
    }
    const tab = newUntitled();
    tabs = [...tabs, tab];
    activeId = tab.id;
    void tick().then(() => editor?.focus());
  }

  async function chooseFiles(): Promise<void> {
    if (!isDesktop()) return showToast(t("desktopFileOnly"), "error");
    const selected = await openDialog({
      multiple: true,
      directory: false,
      filters: [{ name: "Markdown", extensions: ["md", "markdown", "mdown", "mkd"] }],
    });
    if (!selected) return;
    await openPaths(Array.isArray(selected) ? selected : [selected]);
  }

  async function openPaths(paths: string[]): Promise<void> {
    const openPathKeys = new Set(
      tabs.flatMap((tab) => tab.path ? [documentPathKey(tab.path)] : []),
    );
    const requestedPathKeys = new Set(paths.map(documentPathKey));
    const unique = uniqueDocumentPaths(paths, openPathKeys);
    const existing = tabs.find((tab) =>
      tab.path !== null && requestedPathKeys.has(documentPathKey(tab.path))
    );
    if (existing) activeId = existing.id;
    if (!unique.length) return;
    try {
      const opened = await api.openPaths(unique);
      const next = opened.map(fromSnapshot);
      const recentFiles = next.flatMap((tab) => tab.path ? [tab.path] : []);
      mutateSettings((current) => ({
        ...current,
        recentFiles: mergeRecentPaths(recentFiles, current.recentFiles, 20),
      }));
      await persistSettings();
      const replaceBlank = tabs.length === 1 && !tabs[0].path && tabs[0].content.length === 0 && !tabs[0].dirty;
      tabs = replaceBlank ? next : [...tabs, ...next];
      if (next.length) activeId = next[next.length - 1].id;
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  async function chooseWorkspace(): Promise<void> {
    if (!isDesktop()) return showToast(t("desktopFolderOnly"), "error");
    const selected = await openDialog({ multiple: false, directory: true });
    if (typeof selected !== "string") return;
    const requestRevision = ++workspaceRequestRevision;
    try {
      const openedWorkspace = await openWorkspaceSerialized(selected, true, requestRevision);
      if (!openedWorkspace) return;
      workspace = openedWorkspace;
      searchResults = [];
      searchResultQuery = "";
      mutateSettings((current) => ({
        ...current,
        showFileTree: true,
        recentWorkspaces: mergeRecentPaths([openedWorkspace.root], current.recentWorkspaces, 10),
      }));
      await persistSettings();
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  function openWorkspaceSerialized(
    path: string,
    updateSettings: boolean,
    requestRevision: number,
  ): Promise<WorkspaceSnapshot | null> {
    const operation = workspaceOpenTail.then(async () => {
      if (requestRevision !== workspaceRequestRevision) return null;
      try {
        const snapshot = await api.openWorkspace(path, updateSettings);
        return requestRevision === workspaceRequestRevision ? snapshot : null;
      } catch (error) {
        if (requestRevision !== workspaceRequestRevision) return null;
        throw error;
      }
    });
    workspaceOpenTail = operation.then(
      () => undefined,
      () => undefined,
    );
    return operation;
  }

  async function openWorkspaceEntry(entry: WorkspaceEntry): Promise<void> {
    if (!entry.isDir) await openPaths([entry.path]);
  }

  function handleEditorChange(content: Text): void {
    if (!active || interactionLockedTabs.has(active.id)) return;
    const id = active.id;
    documentSerializer.invalidate(id);
    analysisCache.delete(id);
    updateTab(id, (tab) => ({
      ...tab,
      content,
      editorVersion: tab.editorVersion + 1,
      dirty: true,
      saveState: "dirty",
    }));
    scheduleCheckpoint(id);
    scheduleSave(id);
  }

  function scheduleSave(id: string): void {
    const existing = saveTimers.get(id);
    if (existing) clearTimeout(existing);
    saveTimers.set(id, setTimeout(() => {
      const tab = tabs.find((item) => item.id === id);
      if (tab && serializeTab(tab).includes("inkflow-upload://")) {
        scheduleSave(id);
      } else if (tab?.path && tab.dirty && !tab.externalChange && !tab.readOnly && !suspendedSaves.has(id)) {
        void saveTab(id);
      }
    }, settings.autosaveDelayMs));
  }

  function scheduleCheckpoint(id: string): void {
    if (!isDesktop()) return;
    const existing = checkpointTimers.get(id);
    if (existing) clearTimeout(existing);
    checkpointTimers.set(id, setTimeout(() => checkpointNow(id), 2000));
    if (!checkpointMaxTimers.has(id)) {
      checkpointMaxTimers.set(id, setTimeout(() => checkpointNow(id), 15_000));
    }
  }

  function checkpointNow(id: string): void {
    const debounce = checkpointTimers.get(id);
    if (debounce) clearTimeout(debounce);
    checkpointTimers.delete(id);
    const maximum = checkpointMaxTimers.get(id);
    if (maximum) clearTimeout(maximum);
    checkpointMaxTimers.delete(id);
    const tab = tabs.find((item) => item.id === id);
    if (!tab?.dirty) return;
    void api.checkpointDocument({
      documentId: tab.id,
      path: tab.path,
      title: tab.title,
      content: serializeTab(tab),
      kind: "draft",
    }).catch(() => undefined);
  }

  async function saveActive(forceAs = false): Promise<boolean> {
    return active ? saveTab(active.id, forceAs) : false;
  }

  function saveTab(id: string, forceAs = false): Promise<boolean> {
    const previous = saveQueues.get(id) ?? Promise.resolve(false);
    const operation = previous.catch(() => false).then(() => performSaveTab(id, forceAs));
    saveQueues.set(id, operation);
    const cleanup = () => {
      if (saveQueues.get(id) === operation) saveQueues.delete(id);
    };
    void operation.then(cleanup, cleanup);
    return operation;
  }

  async function performSaveTab(id: string, forceAs = false): Promise<boolean> {
    let tab = tabs.find((item) => item.id === id);
    if (!tab || tab.readOnly || !isDesktop() || suspendedSaves.has(id)) return false;
    if (serializeTab(tab).includes("inkflow-upload://")) return false;
    const pendingTimer = saveTimers.get(id);
    if (pendingTimer) clearTimeout(pendingTimer);
    saveTimers.delete(id);
    let path = tab.path;
    if (!path || forceAs) {
      const selected = await saveDialog({
        defaultPath: path ?? tab.title,
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (!selected) return false;
      path = /\.[^.\\/]+$/.test(selected) ? selected : `${selected}.md`;
    }
    updateTab(id, (item) => ({ ...item, saveState: "saving" }));
    tab = tabs.find((item) => item.id === id) ?? tab;
    const request: SaveDocumentRequest = {
      id: tab.id,
      path,
      title: tab.title,
      content: serializeTab(tab),
      encoding: tab.encoding,
      eol: tab.eol,
      hadBom: tab.hadBom,
      expectedRevision: tab.revision,
    };
    const requestVersion = tab.editorVersion;
    try {
      const result = forceAs || !tab.path
        ? await api.saveDocumentAs(request)
        : await api.saveDocument(request);
      const applied = applySaveOutcome(id, result, request.content, requestVersion);
      if (applied && tabs.find((item) => item.id === id)?.dirty) {
        return performSaveTab(id, false);
      }
      return applied;
    } catch (error) {
      updateTab(id, (item) => ({ ...item, saveState: "error" }));
      showToast(messageFromError(error), "error", () => void saveTab(id, forceAs), t("retry"));
      scheduleCheckpoint(id);
      return false;
    }
  }

  function applySaveOutcome(
    id: string,
    result: SaveOutcome,
    savedContent: string,
    savedVersion: number,
  ): boolean {
    if (result.status === "conflict") {
      updateTab(id, (tab) => ({
        ...tab,
        saveState: "dirty",
        externalChange: {
          documentId: id,
          path: result.path,
          kind: result.diskRevision ? "modified" : "deleted",
          revision: result.diskRevision,
        },
      }));
      return false;
    }
    if (result.status === "needsPath") return false;
    let needsResave = false;
    const previousTab = tabs.find((tab) => tab.id === id) ?? null;
    const currentContent = previousTab ? serializeTab(previousTab) : savedContent;
    updateTab(id, (tab) => {
      const applied = applySavedResult(tab, result, savedContent, savedVersion, currentContent);
      needsResave = applied.needsResave;
      return { ...applied.tab, title: fileName(result.path) };
    });
    const savedTab = tabs.find((tab) => tab.id === id) ?? null;
    if (previousTab && savedTab) preserveEditorHistoryForRewrite(id, previousTab, savedTab);
    if (needsResave) scheduleSave(id);
    return true;
  }

  async function reloadActive(): Promise<void> {
    const tab = active;
    if (!tab?.path) return;
    const requestedContent = tab.content;
    try {
      const snapshot = await api.reloadDocument(tab.id);
      updateTab(tab.id, (current) => current.content.eq(requestedContent)
        ? {
            ...fromSnapshot(snapshot),
            mode: current.mode,
            editorVersion: current.editorVersion + 1,
          }
        : {
            ...current,
            externalChange: {
              documentId: current.id,
              path: snapshot.path ?? current.path ?? "",
              kind: "modified",
              revision: snapshot.revision,
            },
          });
      if (conflictDocumentId === tab.id) closeConflictComparison();
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  async function compareExternalChange(): Promise<void> {
    const tab = active;
    if (!tab?.path) return;
    try {
      const snapshot = await api.reloadDocument(tab.id);
      if (!tabs.some((item) => item.id === tab.id)) return;
      conflictDocumentId = tab.id;
      conflictDisk = snapshot;
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  function acceptDiskFromComparison(): void {
    if (!conflictDocumentId || !conflictDisk) return;
    const documentId = conflictDocumentId;
    const snapshot = conflictDisk;
    updateTab(documentId, (tab) => ({
      ...fromSnapshot(snapshot),
      mode: tab.mode,
      editorVersion: tab.editorVersion + 1,
    }));
    closeConflictComparison();
  }

  async function saveLocalFromComparison(): Promise<void> {
    if (!conflictDocumentId) return;
    if (await saveTab(conflictDocumentId, true)) closeConflictComparison();
  }

  function closeConflictComparison(): void {
    conflictDisk = null;
    conflictDocumentId = null;
  }

  async function pollExternalChanges(): Promise<void> {
    if (!isDesktop() || externalPollRunning || tabs.every((tab) => !tab.path)) return;
    externalPollRunning = true;
    try {
      const changes = await api.checkExternalChanges();
      for (const change of changes) {
        const tab = tabs.find((item) => item.id === change.documentId);
        if (!tab || revisionsEqual(tab.externalChange?.revision, change.revision)) continue;
        if (!tab.dirty && change.kind === "modified") {
          const snapshot = await api.reloadDocument(tab.id);
          updateTab(tab.id, (current) => {
            const unchangedSincePoll = !current.dirty
              && current.content.eq(tab.content)
              && revisionsEqual(current.revision, tab.revision);
            if (unchangedSincePoll) {
              return {
                ...fromSnapshot(snapshot),
                mode: current.mode,
                editorVersion: current.editorVersion + 1,
              };
            }
            if (!current.dirty) return current;
            return {
              ...current,
              externalChange: {
                documentId: current.id,
                path: snapshot.path ?? change.path,
                kind: "modified",
                revision: snapshot.revision,
              },
            };
          });
        } else {
          updateTab(tab.id, (item) => ({ ...item, externalChange: change }));
        }
      }
    } catch {
      // Polling is best-effort. Save commands still perform revision checks.
    } finally {
      externalPollRunning = false;
    }
  }

  async function closeTab(id: string): Promise<void> {
    const tab = tabs.find((item) => item.id === id);
    if (!tab) return;
    if (tab.dirty) {
      const shouldSave = isDesktop()
        ? await confirm(t("confirmSave", { title: tab.title }), { title: "InkFlow", kind: "warning", okLabel: t("save"), cancelLabel: t("dontSave") })
        : window.confirm(`Save changes to ${tab.title}?`);
      if (shouldSave && !(await saveTab(id))) return;
    }
    const index = tabs.findIndex((item) => item.id === id);
    const pendingSave = saveQueues.get(id);
    suspendedSaves.add(id);
    clearTabTimers(id);
    tabs = tabs.filter((item) => item.id !== id);
    if (!tabs.length) tabs = [newUntitled()];
    if (activeId === id) activeId = tabs[Math.min(index, tabs.length - 1)].id;
    if (conflictDocumentId === id) closeConflictComparison();
    try {
      await pendingSave?.catch(() => false);
      if (isDesktop()) await api.closeDocument(id);
    } catch (error) {
      showToast(messageFromError(error), "error");
    } finally {
      suspendedSaves.delete(id);
    }
  }

  function clearTabTimers(id: string): void {
    const saveTimer = saveTimers.get(id);
    if (saveTimer) clearTimeout(saveTimer);
    saveTimers.delete(id);
    const checkpointTimer = checkpointTimers.get(id);
    if (checkpointTimer) clearTimeout(checkpointTimer);
    checkpointTimers.delete(id);
    const checkpointMaxTimer = checkpointMaxTimers.get(id);
    if (checkpointMaxTimer) clearTimeout(checkpointMaxTimer);
    checkpointMaxTimers.delete(id);
    saveQueues.delete(id);
    editorStates.delete(id);
    documentSerializer.invalidate(id);
    analysisCache.delete(id);
    if (pendingEditorRewrites.has(id)) {
      const next = new Map(pendingEditorRewrites);
      next.delete(id);
      pendingEditorRewrites = next;
    }
  }

  async function confirmCloseWindow(): Promise<void> {
    const dirty = tabs.filter((tab) => tab.dirty);
    if (dirty.length) {
      const shouldSave = await confirm(t("unsavedCount", { count: dirty.length }), {
        title: "InkFlow",
        kind: "warning",
        okLabel: t("saveAll"),
        cancelLabel: t("cancelClose"),
      });
      if (!shouldSave) return;
      for (const tab of dirty) {
        if (!(await saveTab(tab.id))) return;
      }
    }
    await persistSessionNow().catch(() => undefined);
    closing = true;
    try {
      await getCurrentWindow().destroy();
    } catch (error) {
      closing = false;
      throw error;
    }
  }

  async function pasteImage(documentId: string, file: File, placeholder: string): Promise<void> {
    const sourceTab = tabs.find((tab) => tab.id === documentId);
    if (!sourceTab || !isDesktop()) return;
    try {
      if (file.size > MAX_IMAGE_BYTES) throw new Error(t("imageTooLarge"));
      const data = await fileToDataUrl(file);
      const result = await api.writeAsset({
        documentId,
        documentPath: sourceTab.path,
        sourcePath: null,
        dataBase64: data,
        mimeType: file.type,
      });
      const wrappedPath = /\s/.test(result.markdownPath) ? `<${result.markdownPath}>` : result.markdownPath;
      const markdownImage = `![${file.name.replace(/\.[^.]+$/, "")}](${wrappedPath})`;
      let inserted = false;
      const previousTab = tabs.find((tab) => tab.id === documentId) ?? null;
      const historyEdit = previousTab
        ? uploadPlaceholderEdit(serializeTab(previousTab), placeholder, markdownImage)
        : null;
      updateTab(documentId, (tab) => {
        const content = replaceUploadPlaceholder(serializeTab(tab), placeholder, markdownImage);
        if (content === null) return tab;
        inserted = true;
        documentSerializer.invalidate(documentId);
        analysisCache.delete(documentId);
        return {
          ...tab,
          content: textFromString(content),
          editorVersion: tab.editorVersion + 1,
          dirty: true,
          saveState: "dirty",
        };
      });
      if (inserted) {
        const updatedTab = tabs.find((tab) => tab.id === documentId) ?? null;
        if (previousTab && updatedTab && historyEdit) {
          preserveEditorHistoryForEdits(documentId, previousTab, updatedTab, [historyEdit]);
        }
        scheduleCheckpoint(documentId);
        scheduleSave(documentId);
      }
    } catch (error) {
      let removed = false;
      const previousTab = tabs.find((tab) => tab.id === documentId) ?? null;
      const historyEdit = previousTab
        ? uploadPlaceholderEdit(serializeTab(previousTab), placeholder, "")
        : null;
      updateTab(documentId, (tab) => {
        const content = replaceUploadPlaceholder(serializeTab(tab), placeholder, "");
        if (content === null) return tab;
        removed = true;
        documentSerializer.invalidate(documentId);
        analysisCache.delete(documentId);
        return {
          ...tab,
          content: textFromString(content),
          editorVersion: tab.editorVersion + 1,
          dirty: true,
          saveState: "dirty",
        };
      });
      if (removed) {
        const updatedTab = tabs.find((tab) => tab.id === documentId) ?? null;
        if (previousTab && updatedTab && historyEdit) {
          preserveEditorHistoryForEdits(documentId, previousTab, updatedTab, [historyEdit]);
        }
        scheduleCheckpoint(documentId);
      }
      showToast(messageFromError(error), "error");
    }
  }

  async function createWorkspaceItem(isDir: boolean): Promise<void> {
    if (!workspace) return;
    let name = window.prompt(isDir ? t("folderName") : t("documentName"), isDir ? t("newFolder") : t("untitled"));
    if (!name) return;
    if (!isDir && !/\.[^.]+$/.test(name)) name += ".md";
    try {
      workspace = await api.createWorkspaceEntry(workspace.root, name, isDir);
      if (!isDir) {
        const entry = workspace.entries.find((item) => item.name === name && !item.isDir);
        if (entry) await openPaths([entry.path]);
      }
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  async function renameWorkspaceItem(entry: WorkspaceEntry): Promise<void> {
    const name = window.prompt(t("newName"), entry.name);
    if (!name || name === entry.name) return;
    const affected = tabs.filter((tab) => isPathAffected(tab.path, entry.path, entry.isDir));
    affected.forEach((tab) => suspendedSaves.add(tab.id));
    try {
      await Promise.all(affected.map((tab) => saveQueues.get(tab.id)).filter((value): value is Promise<boolean> => !!value));
      const separator = entry.path.includes("\\") ? "\\" : "/";
      const destination = `${entry.path.slice(0, entry.path.lastIndexOf(separator) + 1)}${name}`;
      workspace = await api.renameWorkspaceEntry(entry.path, name);
      tabs = tabs.map((tab) => {
        if (!tab.path || !isPathAffected(tab.path, entry.path, entry.isDir)) return tab;
        const path = relocatedPath(tab.path, entry.path, destination, entry.isDir);
        return { ...tab, path, title: tab.path === entry.path ? fileName(path) : tab.title };
      });
    } catch (error) {
      showToast(messageFromError(error), "error");
    } finally {
      affected.forEach((tab) => {
        suspendedSaves.delete(tab.id);
        if (tabs.find((item) => item.id === tab.id)?.dirty) scheduleSave(tab.id);
      });
    }
  }

  async function deleteWorkspaceItem(entry: WorkspaceEntry): Promise<void> {
    const accepted = isDesktop()
      ? await confirm(t("moveTrashConfirm", { name: entry.name }), { title: "InkFlow", kind: "warning", okLabel: t("moveTrash"), cancelLabel: t("cancel") })
      : window.confirm(`Delete ${entry.name}?`);
    if (!accepted) return;
    const affected = tabs.filter((tab) => isPathAffected(tab.path, entry.path, entry.isDir));
    const affectedIds = new Set(affected.map((tab) => tab.id));
    setTabsInteractionLocked(affectedIds, true);
    try {
      for (const tab of affected.filter((tab) => tab.dirty)) {
        if (!(await saveTab(tab.id))) return;
      }
      affected.forEach((tab) => suspendedSaves.add(tab.id));
      try {
        workspace = await api.trashWorkspaceEntry(entry.path);
        if (isDesktop()) {
          const closeResults = await Promise.allSettled(affected.map((tab) => api.closeDocument(tab.id)));
          const closeFailure = closeResults.find((result) => result.status === "rejected");
          if (closeFailure?.status === "rejected") showToast(messageFromError(closeFailure.reason), "error");
        }
        tabs = withoutTabsById(tabs, affectedIds);
        for (const tab of affected) {
          clearTabTimers(tab.id);
          if (conflictDocumentId === tab.id) closeConflictComparison();
        }
        if (!tabs.length) tabs = [newUntitled()];
        if (!tabs.some((tab) => tab.id === activeId)) activeId = tabs[0].id;
      } finally {
        affected.forEach((tab) => suspendedSaves.delete(tab.id));
      }
    } catch (error) {
      showToast(messageFromError(error), "error");
    } finally {
      setTabsInteractionLocked(affectedIds, false);
    }
  }

  async function refreshWorkspace(): Promise<void> {
    if (!workspace) return;
    try { workspace = await api.refreshWorkspace(); }
    catch (error) { showToast(messageFromError(error), "error"); }
  }

  async function searchWorkspace(query: string): Promise<void> {
    const revision = ++searchRevision;
    if (!workspace || !query.trim()) {
      searchResults = [];
      searchResultQuery = query;
      searching = false;
      return;
    }
    searching = true;
    searchResults = [];
    searchResultQuery = "";
    try {
      const results = await api.searchWorkspace({ root: workspace.root, query, caseSensitive: false, limit: 500 });
      if (revision === searchRevision) {
        searchResults = results;
        searchResultQuery = query;
      }
    } catch (error) {
      if (revision === searchRevision) {
        searchResults = [];
        searchResultQuery = query;
        showToast(messageFromError(error), "error");
      }
    } finally {
      if (revision === searchRevision) searching = false;
    }
  }

  async function openSearchHit(hit: SearchHit): Promise<void> {
    await openPaths([hit.path]);
    const tab = tabs.find((item) => item.path === hit.path);
    if (tab) activeId = tab.id;
    searchOpen = false;
    await tick();
    editor?.goToLine(hit.line);
  }

  async function exportHtml(): Promise<void> {
    if (!active || !isDesktop()) return;
    const output = await saveDialog({ defaultPath: `${stripExtension(active.title)}.html`, filters: [{ name: "HTML", extensions: ["html"] }] });
    if (!output) return;
    try {
      const renderedHtml = await prepareExportHtml(active);
      const request: ExportRequest = { title: active.title, renderedHtml, outputPath: output, pageSize: "A4", landscape: false };
      const result = await api.exportHtml(request);
      showToast(t("exportedTo", { path: result.path ?? output }));
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  async function exportPdf(): Promise<void> {
    if (!active || !isDesktop()) return;
    const output = await saveDialog({
      defaultPath: `${stripExtension(active.title)}.pdf`,
      filters: [{ name: "PDF", extensions: ["pdf"] }],
    });
    if (!output) return;
    try {
      printHtml = await prepareExportHtml(active);
      await tick();
      document.body.classList.add("printing");
      await tick();
      // A failed or stalled web font must never strand the whole editor in print mode.
      void document.body.offsetHeight;
      await waitForPromiseOrTimeout(document.fonts.ready, 2_000);
      await api.exportPdf({ title: active.title, renderedHtml: printHtml, outputPath: output, pageSize: "A4", landscape: false });
      showToast(t("exportedFile", { name: fileName(output) }));
    } catch (error) {
      showToast(messageFromError(error), "error");
    } finally {
      document.body.classList.remove("printing");
      printHtml = "";
    }
  }

  async function prepareExportHtml(tab: DocumentTab): Promise<string> {
    const rawRendered = await renderInWorker(serializeTab(tab));
    const rendered = tab.allowRemoteImages ? rawRendered : blockRemoteImageRequests(rawRendered);
    const documentNode = new DOMParser().parseFromString(`<main>${rendered}</main>`, "text/html");
    for (const image of Array.from(documentNode.querySelectorAll<HTMLImageElement>("img"))) {
      const blockedSource = image.getAttribute("data-inkflow-remote-src");
      if (blockedSource) {
        image.replaceWith(documentNode.createTextNode(`[Remote image blocked: ${image.alt || blockedSource}]`));
        continue;
      }
      const source = image.getAttribute("src") ?? "";
      if (isRemoteImageSource(source)) {
        if (!tab.allowRemoteImages) image.replaceWith(documentNode.createTextNode(`[Remote image blocked: ${image.alt || source}]`));
      } else if (source && isDesktop()) {
        try { image.src = await api.loadResource(tab.id, source); }
        catch { image.replaceWith(documentNode.createTextNode(`[Missing image: ${image.alt || source}]`)); }
      }
    }
    const mermaidBlocks = Array.from(documentNode.querySelectorAll<HTMLElement>("pre > code.language-mermaid"));
    const mermaid = mermaidBlocks.length ? (await import("mermaid")).default : null;
    mermaid?.initialize({ startOnLoad: false, securityLevel: "strict", theme: "neutral", fontFamily: settings.editorFont });
    for (const code of mermaidBlocks) {
      const pre = code.parentElement;
      if (!pre) continue;
      if (
        !tab.allowRemoteImages
        && await hasRemoteMermaidImageReference(code.textContent ?? "")
      ) {
        pre.classList.add("render-error");
        pre.setAttribute("data-error", "Remote Mermaid image blocked");
        continue;
      }
      try {
        const result = await mermaid!.render(`inkflow-export-${crypto.randomUUID()}`, code.textContent ?? "");
        const figure = documentNode.createElement("figure");
        figure.className = "mermaid-diagram";
        figure.innerHTML = tab.allowRemoteImages
          ? result.svg
          : blockRemoteImageRequests(result.svg);
        pre.replaceWith(figure);
      } catch { /* Preserve the source block on render errors. */ }
    }
    return documentNode.querySelector("main")?.innerHTML ?? rendered;
  }

  async function openRecovery(): Promise<void> {
    recoveryOpen = true;
    recoveryLoading = true;
    try { recoveryEntries = isDesktop() ? await api.listRecovery() : []; }
    catch (error) { showToast(messageFromError(error), "error"); }
    finally { recoveryLoading = false; }
  }

  async function restoreRecovery(entry: RecoveryEntry): Promise<void> {
    try {
      const snapshot = await api.restoreRevision(entry.id);
      const tab = newUntitled(snapshot.content, `${stripExtension(entry.title)}（已恢复）.md`);
      tabs = [...tabs, tab];
      activeId = tab.id;
      recoveryOpen = false;
    } catch (error) { showToast(messageFromError(error), "error"); }
  }

  async function deleteRecovery(entry: RecoveryEntry): Promise<void> {
    try {
      await api.deleteRecovery(entry.id);
      recoveryEntries = recoveryEntries.filter((item) => item.id !== entry.id);
    } catch (error) { showToast(messageFromError(error), "error"); }
  }

  async function saveSettings(next: SettingsV1): Promise<void> {
    mutateSettings((current) => mergeSettingsDialogChanges(current, next));
    if (!isDesktop()) {
      settingsOpen = false;
      return;
    }
    if (!settingsHydration.requestPersistence()) {
      settingsOpen = false;
      return;
    }
    try {
      await settingsWriter.enqueue(settings);
      settingsOpen = false;
    } catch (error) { showToast(messageFromError(error), "error"); }
  }

  async function persistSettings(): Promise<void> {
    if (!isDesktop()) return;
    if (!settingsHydration.requestPersistence()) return;
    try { await settingsWriter.enqueue(settings); }
    catch { /* Layout persistence should never interrupt writing. */ }
  }

  function buildSession(): SessionV1 {
    return buildSessionSnapshot(workspace?.root ?? null, tabs, activeId);
  }

  function sessionKey(session: SessionV1): string {
    return JSON.stringify(session);
  }

  function scheduleSessionPersistence(session: SessionV1): void {
    if (!sessionReady || !isDesktop()) return;
    const key = sessionKey(session);
    if (key === pendingSessionKey) return;
    if (key === lastSessionKey && pendingSessionKey === null) return;
    pendingSessionKey = key;
    scheduleSessionWrite(session, key, 350, SESSION_WRITE_RETRIES);
  }

  function scheduleSessionWrite(
    session: SessionV1,
    key: string,
    delayMs: number,
    retriesRemaining: number,
  ): void {
    if (sessionTimer) clearTimeout(sessionTimer);
    sessionTimer = setTimeout(() => {
      sessionTimer = null;
      void sessionWriter.enqueue(session).then(
        () => {
          lastSessionKey = key;
          if (pendingSessionKey === key) pendingSessionKey = null;
        },
        () => {
          if (pendingSessionKey !== key) return;
          if (retriesRemaining === 0) {
            pendingSessionKey = null;
            return;
          }
          const attempt = SESSION_WRITE_RETRIES - retriesRemaining + 1;
          scheduleSessionWrite(session, key, attempt * 1_000, retriesRemaining - 1);
        },
      );
    }, delayMs);
  }

  async function persistSessionNow(): Promise<void> {
    if (!sessionReady || !isDesktop()) return;
    if (sessionTimer) clearTimeout(sessionTimer);
    sessionTimer = null;
    const session = buildSession();
    const key = sessionKey(session);
    pendingSessionKey = key;
    try {
      await sessionWriter.enqueue(session);
      lastSessionKey = key;
    } finally {
      if (pendingSessionKey === key) pendingSessionKey = null;
    }
  }

  function mutateSettings(mutation: ValueMutation<SettingsV1>): void {
    settings = settingsHydration.apply(settings, mutation);
  }

  function mergeSettingsDialogChanges(current: SettingsV1, next: SettingsV1): SettingsV1 {
    return {
      ...current,
      locale: next.locale,
      theme: next.theme,
      pageWidth: next.pageWidth,
      fontSize: next.fontSize,
      lineHeight: next.lineHeight,
      editorFont: next.editorFont,
      codeFont: next.codeFont,
      autosaveDelayMs: next.autosaveDelayMs,
      typewriterMode: next.typewriterMode,
    };
  }

  function toggleFileTree(): void {
    const showFileTree = !settings.showFileTree;
    mutateSettings((current) => ({ ...current, showFileTree }));
    void persistSettings();
  }

  function toggleOutline(): void {
    const showOutline = !settings.showOutline;
    mutateSettings((current) => ({ ...current, showOutline }));
    void persistSettings();
  }

  function toggleFocus(): void {
    const focusMode = !settings.focusMode;
    mutateSettings((current) => ({ ...current, focusMode }));
    void persistSettings();
  }

  function toggleTypewriter(): void {
    const typewriterMode = !settings.typewriterMode;
    mutateSettings((current) => ({ ...current, typewriterMode }));
    void persistSettings();
  }

  function setMode(mode: DocumentTab["mode"]): void {
    if (active) updateTab(active.id, (tab) => ({ ...tab, mode }));
  }

  function goToOutline(item: OutlineItem): void {
    if (active.mode === "preview") setMode("live");
    void tick().then(() => editor?.goToLine(item.line));
  }

  function updateTab(id: string, transform: (tab: DocumentTab) => DocumentTab): void {
    tabs = tabs.map((tab) => tab.id === id ? transform(tab) : tab);
  }

  function storeEditorState(documentId: string, state: unknown): void {
    if (!tabs.some((tab) => tab.id === documentId)) return;
    if (state == null) editorStates.delete(documentId);
    else editorStates.set(documentId, state);
  }

  function preserveEditorHistoryForRewrite(
    documentId: string,
    previous: DocumentTab,
    nextTab: DocumentTab,
  ): void {
    const previousContent = serializeTab(previous);
    const nextContent = serializeTab(nextTab);
    const edits = imageRewriteEditsBetween(previousContent, nextContent);
    preserveEditorHistoryForEdits(documentId, previous, nextTab, edits);
  }

  function preserveEditorHistoryForEdits(
    documentId: string,
    previous: DocumentTab,
    nextTab: DocumentTab,
    edits: TextEdit[],
  ): void {
    if (previous.content.eq(nextTab.content)) return;
    const previousContent = serializeTab(previous);
    const nextContent = serializeTab(nextTab);
    if (applyTextEdits(previousContent, edits) !== nextContent) return;

    const cached = editorStates.get(documentId);
    if (cached != null) {
      editorStates.set(
        documentId,
        rebaseCachedEditorState(
          cached,
          previousContent,
          previous.editorVersion,
          nextContent,
          nextTab.editorVersion,
          edits,
        ),
      );
      return;
    }
    if (documentId !== activeId || previous.mode === "preview") return;
    const pending = new Map(pendingEditorRewrites);
    pending.set(documentId, {
      previousDoc: previous.content,
      nextDoc: nextTab.content,
      documentVersion: nextTab.editorVersion,
      edits,
    });
    pendingEditorRewrites = pending;
  }

  function handleEditorHistoryRewriteApplied(
    documentId: string,
    documentVersion: number,
  ): void {
    const pending = pendingEditorRewrites.get(documentId);
    if (!pending || pending.documentVersion !== documentVersion) return;
    const next = new Map(pendingEditorRewrites);
    next.delete(documentId);
    pendingEditorRewrites = next;
  }

  function setTabsInteractionLocked(documentIds: Iterable<string>, locked: boolean): void {
    const next = new Set(interactionLockedTabs);
    for (const documentId of documentIds) {
      if (locked) next.add(documentId);
      else next.delete(documentId);
    }
    interactionLockedTabs = next;
  }

  function buildCommands(): PaletteCommand[] {
    return [
      { id: "new", label: t("newDocument"), shortcut: "Ctrl+N", section: "File", run: createDocument },
      { id: "open", label: t("openFile"), shortcut: "Ctrl+O", section: "File", run: chooseFiles },
      { id: "folder", label: t("openFolder"), section: "File", run: chooseWorkspace },
      { id: "save", label: t("save"), shortcut: "Ctrl+S", section: "File", run: () => saveActive() },
      { id: "save-as", label: t("saveAs"), shortcut: "Ctrl+Shift+S", section: "File", run: () => saveActive(true) },
      { id: "html", label: t("exportHtml"), section: "Export", run: exportHtml },
      { id: "pdf", label: t("exportPdf"), section: "Export", run: exportPdf },
      { id: "live", label: t("liveMode"), section: "View", run: () => setMode("live") },
      { id: "source", label: t("sourceMode"), section: "View", run: () => setMode("source") },
      { id: "preview", label: t("previewMode"), section: "View", run: () => setMode("preview") },
      { id: "focus", label: t("focusMode"), shortcut: "F11", section: "View", run: toggleFocus },
      { id: "typewriter", label: t("typewriterMode"), section: "View", run: toggleTypewriter },
      { id: "find", label: t("findDocument"), shortcut: "Ctrl+F", section: "Search", run: () => editor?.openFind() },
      { id: "workspace-search", label: t("workspaceSearch"), shortcut: "Ctrl+Shift+F", section: "Search", run: () => searchOpen = true },
      { id: "recovery", label: t("recovery"), section: "File", run: openRecovery },
      { id: "settings", label: t("settings"), section: "App", run: () => settingsOpen = true },
    ];
  }

  function buildQuickOpenCommands(): PaletteCommand[] {
    const seen = new Set<string>();
    const available: PaletteCommand[] = [];
    for (const entry of workspace?.entries ?? []) {
      if (entry.isDir) continue;
      seen.add(entry.path.toLowerCase());
      available.push({
        id: `workspace:${entry.path}`,
        label: entry.name,
        section: entry.path.replace(workspace?.root ?? "", "").replace(/^[\\/]/, ""),
        run: () => openWorkspaceEntry(entry),
      });
    }
    for (const path of settings.recentFiles) {
      if (seen.has(path.toLowerCase())) continue;
      available.push({
        id: `recent:${path}`,
        label: fileName(path),
        section: path,
        run: () => openPaths([path]),
      });
    }
    available.push({
      id: "browse",
      label: t("openFile"),
      shortcut: "Ctrl+O",
      section: "File",
      run: chooseFiles,
    });
    return available;
  }

  function handleGlobalKey(event: KeyboardEvent): void {
    if (event.defaultPrevented || event.isComposing) return;
    const target = event.target instanceof Element ? event.target : null;
    if (target?.closest('[aria-modal="true"],.search-panel')) return;
    const mod = event.ctrlKey || event.metaKey;
    if (mod && event.shiftKey && event.key.toLowerCase() === "p") { event.preventDefault(); paletteOpen = true; }
    else if (mod && event.shiftKey && event.key.toLowerCase() === "f") { event.preventDefault(); searchOpen = true; }
    else if (mod && event.shiftKey && event.key.toLowerCase() === "s") { event.preventDefault(); void saveActive(true); }
    else if (mod && event.key.toLowerCase() === "s") { event.preventDefault(); void saveActive(); }
    else if (mod && event.key.toLowerCase() === "o") { event.preventDefault(); void chooseFiles(); }
    else if (mod && event.key.toLowerCase() === "n") { event.preventDefault(); createDocument(); }
    else if (mod && event.key.toLowerCase() === "p") { event.preventDefault(); quickOpen = true; }
    else if (event.key === "F11") { event.preventDefault(); toggleFocus(); }
    else if (event.key === "Escape" && tabListOpen) tabListOpen = false;
    else if (event.key === "Escape" && overflowOpen) overflowOpen = false;
    else if (event.key === "Escape" && settings.focusMode) toggleFocus();
  }

  function resolveTheme(theme: SettingsV1["theme"]): "light" | "dark" {
    if (theme === "light" || theme === "dark") return theme;
    return typeof window !== "undefined" && window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }

  function saveStateLabel(state: DocumentTab["saveState"]): string {
    if (state === "error") return t("saveError");
    return t(state);
  }

  function showToast(
    message: string,
    kind: "info" | "error" = "info",
    action: (() => void) | null = null,
    actionLabel = "",
  ): void {
    if (kind === "info" && toastPersistent && toastKind === "error") return;
    const revision = ++toastRevision;
    toast = message;
    toastKind = kind;
    toastPersistent = kind === "error";
    toastAction = action;
    toastActionLabel = actionLabel;
    if (!toastPersistent) setTimeout(() => {
      if (revision === toastRevision) dismissToast();
    }, 3500);
  }

  function dismissToast(): void {
    toastRevision += 1;
    toast = "";
    toastKind = "info";
    toastPersistent = false;
    toastAction = null;
    toastActionLabel = "";
  }

  function runToastAction(): void {
    const action = toastAction;
    dismissToast();
    action?.();
  }

  function dismissWelcome(): void {
    if (!active) return;
    welcomeDismissed = new Set(welcomeDismissed).add(active.id);
    void tick().then(() => editor?.focus());
  }

  function revealActiveTab(id: string): void {
    void tick().then(() => {
      Array.from(document.querySelectorAll<HTMLElement>("[data-tab-id]"))
        .find((element) => element.dataset.tabId === id)
        ?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
    });
  }

  function toggleOverflow(trigger: HTMLElement): void {
    overflowOpen = !overflowOpen;
    tabListOpen = false;
    if (!overflowOpen) return;
    menuReturnFocus = trigger;
    void tick().then(() => document.querySelector<HTMLButtonElement>(".app-menu button")?.focus());
  }

  function toggleTabList(trigger: HTMLElement): void {
    tabListOpen = !tabListOpen;
    overflowOpen = false;
    if (!tabListOpen) return;
    tabListReturnFocus = trigger;
    void tick().then(() => {
      const buttons = Array.from(document.querySelectorAll<HTMLButtonElement>(".tab-list-menu button"));
      (buttons[tabs.findIndex((tab) => tab.id === activeId)] ?? buttons[0])?.focus();
    });
  }

  function activateTabFromList(id: string): void {
    activeId = id;
    tabListOpen = false;
    void tick().then(() => {
      const selected = Array.from(document.querySelectorAll<HTMLElement>("[data-tab-id]"))
        .find((element) => element.dataset.tabId === id);
      (selected ?? tabListReturnFocus)?.focus();
    });
  }

  function menuKeydown(event: KeyboardEvent, close: () => void, returnFocus: HTMLElement | null): void {
    if (event.isComposing) return;
    const menu = event.currentTarget as HTMLElement;
    const buttons = Array.from(menu.querySelectorAll<HTMLButtonElement>("button:not([disabled])"));
    const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
    let next = current;
    if (event.key === "ArrowDown") next = Math.min(current + 1, buttons.length - 1);
    else if (event.key === "ArrowUp") next = Math.max(current - 1, 0);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = buttons.length - 1;
    else if (event.key === "Escape") {
      event.preventDefault();
      close();
      returnFocus?.focus();
      return;
    } else return;
    event.preventDefault();
    buttons[Math.max(0, next)]?.focus();
  }

  function tabKeydown(event: KeyboardEvent, index: number): void {
    let next = index;
    if (event.key === "ArrowLeft") next = Math.max(0, index - 1);
    else if (event.key === "ArrowRight") next = Math.min(tabs.length - 1, index + 1);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = tabs.length - 1;
    else if (event.key === "Enter" || event.key === " ") next = index;
    else return;
    event.preventDefault();
    activeId = tabs[next].id;
    void tick().then(() => document.querySelector<HTMLElement>(`[data-tab-index="${next}"]`)?.focus());
  }

  function serializeTab(tab: DocumentTab): string {
    return documentSerializer.serialize(tab);
  }

  function fileName(path: string): string { return path.split(/[\\/]/).pop() || "Untitled.md"; }
  function stripExtension(name: string): string { return name.replace(/\.[^.]+$/, ""); }
  function revisionsEqual(left: { hash: string } | null | undefined, right: { hash: string } | null | undefined): boolean { return !!left && !!right && left.hash === right.hash; }
  function mergeRecentPaths(preferred: string[], existing: string[], limit: number): string[] {
    const seen = new Set<string>();
    return [...preferred, ...existing].filter((path) => {
      const key = path.replace(/\//g, "\\").toLocaleLowerCase();
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    }).slice(0, limit);
  }
  function fileToDataUrl(file: File): Promise<string> { return new Promise((resolve, reject) => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result)); reader.onerror = () => reject(reader.error); reader.readAsDataURL(file); }); }
</script>

<div class:focus-mode={settings.focusMode} class="app-shell">
  {#if !settings.focusMode}
    <header class="app-bar">
      <div class="bar-left">
        <button class="icon-button" data-app-menu-trigger title="Menu" on:click={(event) => toggleOverflow(event.currentTarget)}><Menu size={17}/></button>
        <button class="icon-button" class:active={settings.showFileTree} title={t("fileTree")} on:click={toggleFileTree}>{#if settings.showFileTree}<PanelLeftClose size={17}/>{:else}<PanelLeftOpen size={17}/>{/if}</button>
      </div>

      <div class="document-tabs" class:single={tabs.length === 1} role="tablist" aria-label="Open documents">
        {#each tabs as tab, index (tab.id)}
          <div data-tab-id={tab.id} data-tab-index={index} role="tab" aria-selected={tab.id === activeId} tabindex={tab.id === activeId ? 0 : -1} class:active={tab.id === activeId} class="document-tab" on:click={() => activeId = tab.id} on:keydown={(event) => tabKeydown(event, index)} title={tab.path ?? tab.title}>
            <span class:dirty={tab.dirty}>{tab.dirty ? "●" : ""}</span><span>{tab.title}</span>
            {#if tabs.length > 1}<button class="tab-close" title={t("close")} on:keydown|stopPropagation on:click|stopPropagation={() => closeTab(tab.id)}><X size={13}/></button>{/if}
          </div>
        {/each}
      </div>

      <div class="bar-right">
        <div class={`save-state ${active?.saveState ?? "saved"}`} title={active?.saveState}>{#if active?.saveState === "saving"}<span class="spinner"></span>{:else if active?.saveState === "saved"}<Check size={13}/>{/if}<span>{active ? saveStateLabel(active.saveState) : ""}</span></div>
        <div class="mode-switch" role="group" aria-label="Editor mode">
          <button class:active={active?.mode === "live"} title={t("liveMode")} on:click={() => setMode("live")}><AlignLeft size={15}/></button>
          <button class:active={active?.mode === "source"} title={t("sourceMode")} on:click={() => setMode("source")}><Braces size={15}/></button>
          <button class:active={active?.mode === "preview"} title={t("previewMode")} on:click={() => setMode("preview")}><BookOpen size={15}/></button>
        </div>
        <button class="icon-button" data-workspace-search-trigger title={t("search")} on:click={() => searchOpen = true}><Search size={16}/></button>
        <button class="icon-button" class:active={settings.showOutline} title={t("outline")} on:click={toggleOutline}>{#if settings.showOutline}<PanelRightClose size={17}/>{:else}<PanelRightOpen size={17}/>{/if}</button>
        {#if tabs.length > 1}<button class="icon-button" data-tab-list-trigger class:active={tabListOpen} title={t("allTabs")} on:click={(event) => toggleTabList(event.currentTarget)}><ChevronDown size={17}/></button>{/if}
        <button class="icon-button" data-app-menu-trigger title="More" on:click={(event) => toggleOverflow(event.currentTarget)}><MoreHorizontal size={18}/></button>
      </div>

      {#if tabListOpen}
        <div class="tab-list-menu" role="menu" tabindex="-1" on:keydown={(event) => menuKeydown(event, () => tabListOpen = false, tabListReturnFocus)}>
          {#each tabs as tab (tab.id)}
            <button class:active={tab.id === activeId} on:click={() => activateTabFromList(tab.id)}><span>{tab.dirty ? "●" : ""}</span><span>{tab.title}</span></button>
          {/each}
        </div>
      {/if}

      {#if overflowOpen}
        <div class="app-menu" role="menu" tabindex="-1" on:keydown={(event) => menuKeydown(event, () => overflowOpen = false, menuReturnFocus)}>
          <button on:click={() => { overflowOpen = false; createDocument(); }}><FilePlus2 size={15}/>{t("newDocument")}<kbd>Ctrl+N</kbd></button>
          <button on:click={() => { overflowOpen = false; void chooseFiles(); }}><Files size={15}/>{t("openFile")}<kbd>Ctrl+O</kbd></button>
          <button on:click={() => { overflowOpen = false; void chooseWorkspace(); }}><FolderOpen size={15}/>{t("openFolder")}</button>
          <hr/>
          <button on:click={() => { overflowOpen = false; void saveActive(); }}><Save size={15}/>{t("save")}<kbd>Ctrl+S</kbd></button>
          <button on:click={() => { overflowOpen = false; void exportHtml(); }}><FileDown size={15}/>{t("exportHtml")}</button>
          <button on:click={() => { overflowOpen = false; void exportPdf(); }}><FileDown size={15}/>{t("exportPdf")}</button>
          <hr/>
          <button on:click={() => { overflowOpen = false; toggleFocus(); }}><Focus size={15}/>{t("focusMode")}<kbd>F11</kbd></button>
          <button on:click={() => { overflowOpen = false; toggleTypewriter(); }}><MoveVertical size={15}/>{t("typewriterMode")}</button>
          <button on:click={() => { overflowOpen = false; void openRecovery(); }}><History size={15}/>{t("recovery")}</button>
          <button on:click={() => { overflowOpen = false; settingsOpen = true; }}><Settings size={15}/>{t("settings")}</button>
        </div>
      {/if}
    </header>
  {/if}

  {#if active?.externalChange}
    <div class="conflict-banner">
      <span>{active.externalChange.kind === "deleted" ? t("fileDeleted") : t("externalChanged")}</span>
      {#if active.externalChange.kind === "modified"}<button on:click={compareExternalChange}>{t("compare")}</button><button on:click={reloadActive}>{t("reload")}</button>{/if}
      <button on:click={() => saveTab(active.id, true)}>{t("saveAs")}</button>
      <button class="banner-close" title="Dismiss" on:click={() => updateTab(active.id, (tab) => ({ ...tab, externalChange: null }))}><X size={14}/></button>
    </div>
  {/if}

  {#if hasBlockedRemoteImages && !active?.externalChange}
    <div class="remote-banner">
      <span>{t("remoteBlocked")}</span>
      <button on:click={() => updateTab(active.id, (tab) => ({ ...tab, allowRemoteImages: true }))}>{t("loadForDocument")}</button>
    </div>
  {/if}

  <main class="workspace-grid" class:without-bar={settings.focusMode} style={`--left:${settings.showFileTree && !settings.focusMode ? 248 : 0}px;--right:${settings.showOutline && !settings.focusMode ? 230 : 0}px`}>
    {#if settings.showFileTree && !settings.focusMode}
      <FileSidebar
        {locale}
        {workspace}
        activePath={active?.path ?? null}
        onOpen={openWorkspaceEntry}
        onCreate={createWorkspaceItem}
        onRefresh={refreshWorkspace}
        onRename={renameWorkspaceItem}
        onDelete={deleteWorkspaceItem}
      />
    {/if}

    <section class="writing-area">
      {#if active}
        {#key active.id}
          {#if active.mode === "preview"}
            <MarkdownPreview bind:this={preview} value={serializeTab(active)} documentId={active.id} allowRemoteImages={active.allowRemoteImages} pageWidth={settings.pageWidth} fontSize={settings.fontSize} lineHeight={settings.lineHeight} editorFont={settings.editorFont} theme={effectiveTheme}/>
          {:else}
            <MarkdownEditor bind:this={editor} {locale} value={active.content} documentId={active.id} documentVersion={active.editorVersion} mode={active.mode} readOnly={active.readOnly || interactionLockedTabs.has(active.id)} allowRemoteImages={active.allowRemoteImages} {settings} onChange={handleEditorChange} onPasteImage={pasteImage} loadResource={api.loadResource} cachedState={editorStates.get(active.id)} historyRewrite={pendingEditorRewrites.get(active.id)} onStateChange={storeEditorState} onHistoryRewriteApplied={handleEditorHistoryRewriteApplied}/>
          {/if}
        {/key}
      {/if}
      <div class="document-stats" aria-label="Document statistics">{stats.words} {t("words")} · {stats.lines} {t("lines")}</div>
      {#if showWelcome}
        <section class="welcome-card" aria-label={t("welcomeTitle")}>
          <div class="welcome-mark">I</div>
          <h1>{t("welcomeTitle")}</h1>
          <p>{t("welcomeHint")}</p>
          <div class="welcome-actions">
            <button class="primary" on:click={dismissWelcome}>{t("newDocument")}</button>
            <button on:click={() => void chooseFiles()}>{t("openFile")}</button>
            <button on:click={() => void chooseWorkspace()}>{t("openFolder")}</button>
          </div>
          <small>{t("commonShortcuts")}</small>
        </section>
      {/if}
    </section>

    {#if settings.showOutline && !settings.focusMode}
      <OutlineSidebar {locale} items={outline} onSelect={goToOutline}/>
    {/if}
  </main>

  <WorkspaceSearch {locale} open={searchOpen} workspaceName={workspace?.name ?? ""} {searching} results={searchResults} resultQuery={searchResultQuery} onSearch={searchWorkspace} onOpen={openSearchHit} onClose={() => searchOpen = false}/>
  <CommandPalette open={paletteOpen} {commands} placeholder={t("commandPlaceholder")} emptyText={t("noMatchingCommand")} onClose={() => paletteOpen = false}/>
  <CommandPalette open={quickOpen} commands={quickOpenCommands} placeholder={t("quickOpenPlaceholder")} emptyText={t("noMatchingFile")} ariaLabel="Quick open" onClose={() => quickOpen = false}/>
  <SettingsDialog {locale} open={settingsOpen} {settings} onSave={saveSettings} onClose={() => settingsOpen = false}/>
  <RecoveryDialog {locale} open={recoveryOpen} entries={recoveryEntries} loading={recoveryLoading} onRestore={restoreRecovery} onDelete={deleteRecovery} onClose={() => recoveryOpen = false}/>
  <ConflictDialog {locale} open={conflictDisk !== null && conflictTab !== null} title={conflictTab?.title ?? ""} localContent={conflictTab ? serializeTab(conflictTab) : ""} diskContent={conflictDisk?.content ?? ""} onReload={acceptDiskFromComparison} onSaveAs={saveLocalFromComparison} onClose={closeConflictComparison}/>

  {#if toast}<div class:error={toastKind === "error"} class="toast" role={toastKind === "error" ? "alert" : "status"} aria-live={toastKind === "error" ? "assertive" : "polite"}><span>{toast}</span>{#if toastAction}<button on:click={runToastAction}>{toastActionLabel}</button>{/if}{#if toastPersistent}<button class="toast-close" title={t("close")} on:click={dismissToast}><X size={14}/></button>{/if}</div>{/if}
  {#if printHtml}<article class="print-document">{@html printHtml}</article>{/if}
</div>
