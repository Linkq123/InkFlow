<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
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
  import { renderInWorker } from "./lib/markdown/render-service";
  import { waitForPromiseOrTimeout } from "./lib/async";
  import { applySavedResult, isPathAffected, relocatedPath, replaceUploadPlaceholder, withoutTabsById } from "./lib/document-state";
  import { documentStats, extractOutline, type OutlineItem } from "./lib/stats";

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

  let settings = structuredClone(defaultSettings);
  let tabs: DocumentTab[] = [newUntitled()];
  let activeId = tabs[0].id;
  let workspace: WorkspaceSnapshot | null = null;
  let editor: MarkdownEditor | null = null;
  let preview: MarkdownPreview | null = null;
  let paletteOpen = false;
  let quickOpen = false;
  let overflowOpen = false;
  let searchOpen = false;
  let searchResults: SearchHit[] = [];
  let searching = false;
  let settingsOpen = false;
  let recoveryOpen = false;
  let recoveryEntries: RecoveryEntry[] = [];
  let recoveryLoading = false;
  let conflictDisk: DocumentSnapshot | null = null;
  let toast = "";
  let toastKind: "info" | "error" = "info";
  let printHtml = "";
  let saveTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let checkpointTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let saveQueues = new Map<string, Promise<boolean>>();
  let suspendedSaves = new Set<string>();
  let externalTimer: ReturnType<typeof setInterval> | null = null;
  let unlistenPaths: UnlistenFn | null = null;
  let unlistenClose: UnlistenFn | null = null;
  let closing = false;

  $: active = tabs.find((tab) => tab.id === activeId) ?? tabs[0];
  $: stats = documentStats(active?.content ?? "");
  $: outline = extractOutline(active?.content ?? "");
  $: hasBlockedRemoteImages = !!active && !active.allowRemoteImages && /(?:!\[[^\]]*\]\(|<img\s[^>]*src=["'])https?:\/\//i.test(active.content);
  $: locale = resolveLocale(settings.locale);
  $: effectiveTheme = resolveTheme(settings.theme);
  $: t = (key: Parameters<typeof translate>[1], values: Record<string, string | number> = {}) => translate(locale, key, values);
  $: commands = buildCommands();
  $: quickOpenCommands = buildQuickOpenCommands();
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
    window.addEventListener("keydown", keyHandler);
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const themeHandler = () => settings = { ...settings };
    media.addEventListener("change", themeHandler);
    externalTimer = setInterval(() => void pollExternalChanges(), 2200);
    return () => {
      window.removeEventListener("keydown", keyHandler);
      media.removeEventListener("change", themeHandler);
      if (externalTimer) clearInterval(externalTimer);
      unlistenPaths?.();
      unlistenClose?.();
      for (const timer of saveTimers.values()) clearTimeout(timer);
      for (const timer of checkpointTimers.values()) clearTimeout(timer);
    };
  });

  async function initialize(): Promise<void> {
    if (!isDesktop()) return;
    try {
      settings = await api.getSettings();
      const recovered = await api.listRecovery();
      if (recovered.some((entry) => entry.kind === "draft")) {
        showToast(t("recoverDetected"));
      }
      unlistenPaths = await listen<string[]>("app-open-paths", (event) => void openPaths(event.payload));
      const startupPaths = await api.takeStartupPaths();
      if (startupPaths.length) await openPaths(startupPaths);
      const appWindow = getCurrentWindow();
      unlistenClose = await appWindow.onCloseRequested((event) => {
        if (closing || !tabs.some((tab) => tab.dirty)) return;
        event.preventDefault();
        void confirmCloseWindow();
      });
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  function newUntitled(content = "", title?: string): DocumentTab {
    return {
      id: crypto.randomUUID(),
      path: null,
      title: title ?? translate(resolveLocale(settings.locale), "untitled"),
      content,
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
    };
  }

  function fromSnapshot(snapshot: DocumentSnapshot): DocumentTab {
    return { ...snapshot, dirty: false, saveState: "saved", mode: "live", externalChange: null, allowRemoteImages: false };
  }

  function createDocument(): void {
    const blank = tabs.length === 1 && !tabs[0].path && !tabs[0].content && !tabs[0].dirty;
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
    const unique = paths.filter((path) => !tabs.some((tab) => tab.path === path));
    const existing = tabs.find((tab) => paths.includes(tab.path ?? ""));
    if (existing) activeId = existing.id;
    if (!unique.length) return;
    try {
      const opened = await api.openPaths(unique);
      const next = opened.map(fromSnapshot);
      const replaceBlank = tabs.length === 1 && !tabs[0].path && !tabs[0].content && !tabs[0].dirty;
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
    try {
      workspace = await api.openWorkspace(selected);
      settings = { ...settings, showFileTree: true };
      await persistSettings();
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  async function openWorkspaceEntry(entry: WorkspaceEntry): Promise<void> {
    if (!entry.isDir) await openPaths([entry.path]);
  }

  function handleEditorChange(content: string): void {
    if (!active) return;
    const id = active.id;
    updateTab(id, (tab) => ({ ...tab, content, dirty: true, saveState: "dirty" }));
    scheduleCheckpoint(id);
    scheduleSave(id);
  }

  function scheduleSave(id: string): void {
    const existing = saveTimers.get(id);
    if (existing) clearTimeout(existing);
    saveTimers.set(id, setTimeout(() => {
      const tab = tabs.find((item) => item.id === id);
      if (tab?.content.includes("inkflow-upload://")) {
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
    checkpointTimers.set(id, setTimeout(() => {
      const tab = tabs.find((item) => item.id === id);
      if (!tab?.dirty) return;
      void api.checkpointDocument({
        documentId: tab.id,
        path: tab.path,
        title: tab.title,
        content: tab.content,
        kind: "draft",
      }).catch(() => undefined);
    }, 2000));
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
    if (tab.content.includes("inkflow-upload://")) return false;
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
      content: tab.content,
      encoding: tab.encoding,
      eol: tab.eol,
      hadBom: tab.hadBom,
      expectedRevision: tab.revision,
    };
    try {
      const result = forceAs || !tab.path
        ? await api.saveDocumentAs(request)
        : await api.saveDocument(request);
      const applied = applySaveOutcome(id, result, request.content);
      if (applied && tabs.find((item) => item.id === id)?.dirty) {
        return performSaveTab(id, false);
      }
      return applied;
    } catch (error) {
      updateTab(id, (item) => ({ ...item, saveState: "error" }));
      showToast(messageFromError(error), "error");
      scheduleCheckpoint(id);
      return false;
    }
  }

  function applySaveOutcome(id: string, result: SaveOutcome, savedContent: string): boolean {
    if (result.status === "conflict") {
      updateTab(id, (tab) => ({
        ...tab,
        saveState: "dirty",
        externalChange: { documentId: id, path: result.path, kind: "modified", revision: result.diskRevision },
      }));
      return false;
    }
    if (result.status === "needsPath") return false;
    let needsResave = false;
    updateTab(id, (tab) => {
      const applied = applySavedResult(tab, result, savedContent);
      needsResave = applied.needsResave;
      return { ...applied.tab, title: fileName(result.path) };
    });
    if (needsResave) scheduleSave(id);
    return true;
  }

  async function reloadActive(): Promise<void> {
    if (!active?.path) return;
    try {
      const snapshot = await api.reloadDocument(active.id);
      const previousMode = active.mode;
      updateTab(active.id, () => ({ ...fromSnapshot(snapshot), mode: previousMode }));
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  async function compareExternalChange(): Promise<void> {
    if (!active?.path) return;
    try {
      conflictDisk = await api.reloadDocument(active.id);
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  function acceptDiskFromComparison(): void {
    if (!active || !conflictDisk) return;
    const mode = active.mode;
    updateTab(active.id, () => ({ ...fromSnapshot(conflictDisk!), mode }));
    conflictDisk = null;
  }

  async function saveLocalFromComparison(): Promise<void> {
    if (!active) return;
    if (await saveTab(active.id, true)) conflictDisk = null;
  }

  async function pollExternalChanges(): Promise<void> {
    if (!isDesktop() || tabs.every((tab) => !tab.path)) return;
    try {
      const changes = await api.checkExternalChanges();
      for (const change of changes) {
        const tab = tabs.find((item) => item.id === change.documentId);
        if (!tab || revisionsEqual(tab.externalChange?.revision, change.revision)) continue;
        if (!tab.dirty && change.kind === "modified") {
          const snapshot = await api.reloadDocument(tab.id);
          updateTab(tab.id, () => ({ ...fromSnapshot(snapshot), mode: tab.mode }));
        } else {
          updateTab(tab.id, (item) => ({ ...item, externalChange: change }));
        }
      }
    } catch {
      // Polling is best-effort. Save commands still perform revision checks.
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
    tabs = tabs.filter((item) => item.id !== id);
    if (!tabs.length) tabs = [newUntitled()];
    if (activeId === id) activeId = tabs[Math.min(index, tabs.length - 1)].id;
  }

  async function confirmCloseWindow(): Promise<void> {
    const dirty = tabs.filter((tab) => tab.dirty);
    if (!dirty.length) return;
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
    closing = true;
    await getCurrentWindow().destroy();
  }

  async function pasteImage(documentId: string, file: File, placeholder: string): Promise<void> {
    const sourceTab = tabs.find((tab) => tab.id === documentId);
    if (!sourceTab || !isDesktop()) return;
    try {
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
      updateTab(documentId, (tab) => {
        const content = replaceUploadPlaceholder(tab.content, placeholder, markdownImage);
        if (content === null) return tab;
        inserted = true;
        return { ...tab, content, dirty: true, saveState: "dirty" };
      });
      if (inserted) {
        scheduleCheckpoint(documentId);
        scheduleSave(documentId);
      }
    } catch (error) {
      let removed = false;
      updateTab(documentId, (tab) => {
        const content = replaceUploadPlaceholder(tab.content, placeholder, "");
        if (content === null) return tab;
        removed = true;
        return { ...tab, content, dirty: true, saveState: "dirty" };
      });
      if (removed) scheduleCheckpoint(documentId);
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
    try {
      const affected = tabs.filter((tab) => isPathAffected(tab.path, entry.path, entry.isDir));
      for (const tab of affected.filter((tab) => tab.dirty)) {
        if (!(await saveTab(tab.id))) return;
      }
      affected.forEach((tab) => suspendedSaves.add(tab.id));
      try {
        workspace = await api.trashWorkspaceEntry(entry.path);
      } finally {
        affected.forEach((tab) => suspendedSaves.delete(tab.id));
      }
      const affectedIds = new Set(affected.map((tab) => tab.id));
      tabs = withoutTabsById(tabs, affectedIds);
      for (const tab of affected) {
        const saveTimer = saveTimers.get(tab.id);
        if (saveTimer) clearTimeout(saveTimer);
        saveTimers.delete(tab.id);
        const checkpointTimer = checkpointTimers.get(tab.id);
        if (checkpointTimer) clearTimeout(checkpointTimer);
        checkpointTimers.delete(tab.id);
      }
      if (!tabs.length) tabs = [newUntitled()];
      if (!tabs.some((tab) => tab.id === activeId)) activeId = tabs[0].id;
    } catch (error) {
      showToast(messageFromError(error), "error");
    }
  }

  async function refreshWorkspace(): Promise<void> {
    if (!workspace) return;
    try { workspace = await api.refreshWorkspace(); }
    catch (error) { showToast(messageFromError(error), "error"); }
  }

  async function searchWorkspace(query: string): Promise<void> {
    if (!workspace || !query.trim()) { searchResults = []; return; }
    searching = true;
    try {
      searchResults = await api.searchWorkspace({ root: workspace.root, query, caseSensitive: false, limit: 500 });
    } catch (error) {
      showToast(messageFromError(error), "error");
    } finally {
      searching = false;
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
    const rendered = await renderInWorker(tab.content);
    const documentNode = new DOMParser().parseFromString(`<main>${rendered}</main>`, "text/html");
    for (const image of Array.from(documentNode.querySelectorAll<HTMLImageElement>("img"))) {
      const source = image.getAttribute("src") ?? "";
      if (/^https?:\/\//i.test(source)) {
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
      try {
        const result = await mermaid!.render(`inkflow-export-${crypto.randomUUID()}`, code.textContent ?? "");
        const figure = documentNode.createElement("figure");
        figure.className = "mermaid-diagram";
        figure.innerHTML = result.svg;
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
    try {
      settings = isDesktop() ? await api.updateSettings(next) : next;
      settingsOpen = false;
    } catch (error) { showToast(messageFromError(error), "error"); }
  }

  async function persistSettings(): Promise<void> {
    if (!isDesktop()) return;
    try { settings = await api.updateSettings(settings); }
    catch { /* Layout persistence should never interrupt writing. */ }
  }

  function toggleFileTree(): void {
    settings = { ...settings, showFileTree: !settings.showFileTree };
    void persistSettings();
  }

  function toggleOutline(): void {
    settings = { ...settings, showOutline: !settings.showOutline };
    void persistSettings();
  }

  function toggleFocus(): void {
    settings = { ...settings, focusMode: !settings.focusMode };
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
    const mod = event.ctrlKey || event.metaKey;
    if (mod && event.shiftKey && event.key.toLowerCase() === "p") { event.preventDefault(); paletteOpen = true; }
    else if (mod && event.shiftKey && event.key.toLowerCase() === "f") { event.preventDefault(); searchOpen = true; }
    else if (mod && event.shiftKey && event.key.toLowerCase() === "s") { event.preventDefault(); void saveActive(true); }
    else if (mod && event.key.toLowerCase() === "s") { event.preventDefault(); void saveActive(); }
    else if (mod && event.key.toLowerCase() === "o") { event.preventDefault(); void chooseFiles(); }
    else if (mod && event.key.toLowerCase() === "n") { event.preventDefault(); createDocument(); }
    else if (mod && event.key.toLowerCase() === "p") { event.preventDefault(); quickOpen = true; }
    else if (event.key === "F11") { event.preventDefault(); toggleFocus(); }
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

  function showToast(message: string, kind: "info" | "error" = "info"): void {
    toast = message;
    toastKind = kind;
    setTimeout(() => { if (toast === message) toast = ""; }, 3500);
  }

  function fileName(path: string): string { return path.split(/[\\/]/).pop() || "Untitled.md"; }
  function stripExtension(name: string): string { return name.replace(/\.[^.]+$/, ""); }
  function revisionsEqual(left: { hash: string } | null | undefined, right: { hash: string } | null | undefined): boolean { return !!left && !!right && left.hash === right.hash; }
  function fileToDataUrl(file: File): Promise<string> { return new Promise((resolve, reject) => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result)); reader.onerror = () => reject(reader.error); reader.readAsDataURL(file); }); }
</script>

<div class:focus-mode={settings.focusMode} class="app-shell">
  {#if !settings.focusMode}
    <header class="app-bar">
      <div class="bar-left">
        <button class="icon-button" title="Menu" on:click={() => overflowOpen = !overflowOpen}><Menu size={17}/></button>
        <button class="icon-button" class:active={settings.showFileTree} title={t("fileTree")} on:click={toggleFileTree}>{#if settings.showFileTree}<PanelLeftClose size={17}/>{:else}<PanelLeftOpen size={17}/>{/if}</button>
      </div>

      <div class="document-tabs" class:single={tabs.length === 1}>
        {#each tabs as tab (tab.id)}
          <button class:active={tab.id === activeId} class="document-tab" on:click={() => activeId = tab.id} title={tab.path ?? tab.title}>
            <span class:dirty={tab.dirty}>{tab.dirty ? "●" : ""}</span><span>{tab.title}</span>
            {#if tabs.length > 1}<span class="tab-close" role="button" tabindex="0" on:click|stopPropagation={() => closeTab(tab.id)} on:keydown={(event) => event.key === "Enter" && closeTab(tab.id)}><X size={13}/></span>{/if}
          </button>
        {/each}
      </div>

      <div class="bar-right">
        <div class={`save-state ${active?.saveState ?? "saved"}`} title={active?.saveState}>{#if active?.saveState === "saving"}<span class="spinner"></span>{:else if active?.saveState === "saved"}<Check size={13}/>{/if}<span>{active ? saveStateLabel(active.saveState) : ""}</span></div>
        <div class="mode-switch" role="group" aria-label="Editor mode">
          <button class:active={active?.mode === "live"} title={t("liveMode")} on:click={() => setMode("live")}><AlignLeft size={15}/></button>
          <button class:active={active?.mode === "source"} title={t("sourceMode")} on:click={() => setMode("source")}><Braces size={15}/></button>
          <button class:active={active?.mode === "preview"} title={t("previewMode")} on:click={() => setMode("preview")}><BookOpen size={15}/></button>
        </div>
        <button class="icon-button" title={t("search")} on:click={() => searchOpen = true}><Search size={16}/></button>
        <button class="icon-button" class:active={settings.showOutline} title={t("outline")} on:click={toggleOutline}>{#if settings.showOutline}<PanelRightClose size={17}/>{:else}<PanelRightOpen size={17}/>{/if}</button>
        <button class="icon-button" title="More" on:click={() => overflowOpen = !overflowOpen}><MoreHorizontal size={18}/></button>
      </div>

      {#if overflowOpen}
        <div class="app-menu" role="menu" tabindex="-1" on:mouseleave={() => overflowOpen = false}>
          <button on:click={() => { overflowOpen = false; createDocument(); }}><FilePlus2 size={15}/>{t("newDocument")}<kbd>Ctrl+N</kbd></button>
          <button on:click={() => { overflowOpen = false; void chooseFiles(); }}><Files size={15}/>{t("openFile")}<kbd>Ctrl+O</kbd></button>
          <button on:click={() => { overflowOpen = false; void chooseWorkspace(); }}><FolderOpen size={15}/>{t("openFolder")}</button>
          <hr/>
          <button on:click={() => { overflowOpen = false; void saveActive(); }}><Save size={15}/>{t("save")}<kbd>Ctrl+S</kbd></button>
          <button on:click={() => { overflowOpen = false; void exportHtml(); }}><FileDown size={15}/>{t("exportHtml")}</button>
          <button on:click={() => { overflowOpen = false; void exportPdf(); }}><FileDown size={15}/>{t("exportPdf")}</button>
          <hr/>
          <button on:click={() => { overflowOpen = false; toggleFocus(); }}><Focus size={15}/>{t("focusMode")}<kbd>F11</kbd></button>
          <button on:click={() => { overflowOpen = false; void openRecovery(); }}><History size={15}/>{t("recovery")}</button>
          <button on:click={() => { overflowOpen = false; settingsOpen = true; }}><Settings size={15}/>{t("settings")}</button>
        </div>
      {/if}
    </header>
  {/if}

  {#if active?.externalChange}
    <div class="conflict-banner">
      <span>{active.externalChange.kind === "deleted" ? t("fileDeleted") : t("externalChanged")}</span>
      {#if active.externalChange.kind === "modified"}<button on:click={compareExternalChange}>{t("compare")}</button><button on:click={reloadActive}>{t("reload")}</button><button on:click={() => saveTab(active.id, true)}>{t("saveAs")}</button>{/if}
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
            <MarkdownPreview bind:this={preview} value={active.content} documentId={active.id} allowRemoteImages={active.allowRemoteImages} pageWidth={settings.pageWidth} fontSize={settings.fontSize} lineHeight={settings.lineHeight} editorFont={settings.editorFont} theme={effectiveTheme}/>
          {:else}
            <MarkdownEditor bind:this={editor} {locale} value={active.content} documentId={active.id} mode={active.mode} readOnly={active.readOnly} allowRemoteImages={active.allowRemoteImages} {settings} onChange={handleEditorChange} onPasteImage={pasteImage} loadResource={api.loadResource}/>
          {/if}
        {/key}
      {/if}
      <div class="document-stats" aria-label="Document statistics">{stats.words} {t("words")} · {stats.lines} {t("lines")}</div>
    </section>

    {#if settings.showOutline && !settings.focusMode}
      <OutlineSidebar {locale} items={outline} onSelect={goToOutline}/>
    {/if}
  </main>

  <WorkspaceSearch {locale} open={searchOpen} workspaceName={workspace?.name ?? ""} {searching} results={searchResults} onSearch={searchWorkspace} onOpen={openSearchHit} onClose={() => searchOpen = false}/>
  <CommandPalette open={paletteOpen} {commands} placeholder={t("commandPlaceholder")} emptyText={t("noMatchingCommand")} onClose={() => paletteOpen = false}/>
  <CommandPalette open={quickOpen} commands={quickOpenCommands} placeholder={t("quickOpenPlaceholder")} emptyText={t("noMatchingFile")} ariaLabel="Quick open" onClose={() => quickOpen = false}/>
  <SettingsDialog {locale} open={settingsOpen} {settings} onSave={saveSettings} onClose={() => settingsOpen = false}/>
  <RecoveryDialog {locale} open={recoveryOpen} entries={recoveryEntries} loading={recoveryLoading} onRestore={restoreRecovery} onDelete={deleteRecovery} onClose={() => recoveryOpen = false}/>
  <ConflictDialog {locale} open={conflictDisk !== null} title={active?.title ?? ""} localContent={active?.content ?? ""} diskContent={conflictDisk?.content ?? ""} onReload={acceptDiskFromComparison} onSaveAs={saveLocalFromComparison} onClose={() => conflictDisk = null}/>

  {#if toast}<div class:error={toastKind === "error"} class="toast">{toast}</div>{/if}
  {#if printHtml}<article class="print-document">{@html printHtml}</article>{/if}
</div>
