import { mount, tick, unmount } from "svelte";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { EditorView } from "@codemirror/view";
import { insertNewlineAndIndent, isolateHistory, redo, redoDepth, undo, undoDepth } from "@codemirror/commands";
import type { ExternalChange, RecoveryEntry, SearchHit } from "./lib/api/types";
import imageRewriteFixtures from "../tests/fixtures/image-rewrites.json";
import imageRewriteMerges from "../tests/fixtures/image-rewrite-merges.json";
import { renderMarkdown } from "./lib/markdown/pipeline";
import { collectImageDestinations } from "./lib/markdown/image-destinations";
import * as imageDestinationService from "./lib/markdown/image-destination-service";

const mocks = vi.hoisted(() => ({
  saveDialog: vi.fn(),
  openDialog: vi.fn(async () => null as string | null),
  confirmDialog: vi.fn(async () => true),
  prepareExportDocument: vi.fn(async (_markdown: string, _options: unknown) =>
    "<p>Alpha snapshot</p>"),
  api: {
    takeStartupTargets: vi.fn(),
    openPaths: vi.fn(),
    closeDocument: vi.fn(async () => undefined),
    checkpointDocument: vi.fn(async () => null),
    writeAsset: vi.fn(),
    saveDocument: vi.fn(),
    saveDocumentAs: vi.fn(),
    getSettings: vi.fn(),
    getSession: vi.fn(),
    updateSession: vi.fn(async (session) => session),
    updateSettings: vi.fn(async (settings) => settings),
    listRecovery: vi.fn(async (): Promise<RecoveryEntry[]> => []),
    restoreRevision: vi.fn(),
    markPerformanceReady: vi.fn(async () => true),
    checkExternalChanges: vi.fn(async (): Promise<ExternalChange[]> => []),
    reloadDocument: vi.fn(),
    openWorkspace: vi.fn(),
    openWorkspaceResource: vi.fn(async () => undefined),
    createWorkspaceEntry: vi.fn(),
    trashWorkspaceEntry: vi.fn(),
    refreshWorkspace: vi.fn(),
    renameWorkspaceEntry: vi.fn(),
    searchWorkspace: vi.fn(),
    prepareExportSource: vi.fn(),
    loadExportResource: vi.fn(),
    cancelExportSource: vi.fn(async () => undefined),
    prepareExportDestination: vi.fn(),
    cancelExportDestination: vi.fn(async () => undefined),
    exportHtml: vi.fn(),
    exportPdf: vi.fn(),
    loadResource: vi.fn(),
  },
}));

vi.mock("./lib/api/client", () => ({
  api: mocks.api,
  isDesktop: () => true,
  messageFromError: (error: unknown) =>
    typeof error === "object" && error !== null && "message" in error
      ? String((error as { message: unknown }).message)
      : String(error),
}));

vi.mock("./lib/markdown/export-document", () => ({
  prepareExportDocument: mocks.prepareExportDocument,
}));

vi.mock("./lib/markdown/render-service", () => ({
  renderInWorker: vi.fn(async () => "<p>Preview snapshot</p>"),
  analyzeMarkdownInWorker: vi.fn(async () => ({
    stats: { words: 2, lines: 1, characters: 16 },
    outline: [],
    hasRemoteImages: false,
  })),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  confirm: mocks.confirmDialog,
  open: mocks.openDialog,
  save: mocks.saveDialog,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));

let closeRequestedHandler: ((event: { preventDefault: () => void }) => void | Promise<void>) | null = null;

const appWindow = {
  setTitle: vi.fn(async () => undefined),
  onCloseRequested: vi.fn(async (
    handler: (event: { preventDefault: () => void }) => void | Promise<void>,
  ) => {
    closeRequestedHandler = handler;
    return () => undefined;
  }),
  destroy: vi.fn(async () => undefined),
};

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => appWindow,
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn(async () => undefined),
}));

import App from "./App.svelte";

const settings = {
  schemaVersion: 1,
  locale: "en-US",
  theme: "light",
  pageWidth: 820,
  fontSize: 16,
  lineHeight: 1.75,
  editorFont: "Snapshot Font",
  codeFont: "Cascadia Mono",
  autosaveDelayMs: 750,
  showFileTree: false,
  showOutline: false,
  focusMode: false,
  typewriterMode: false,
  recentFiles: [],
  recentWorkspaces: [],
};

const alphaDocument = {
  id: "alpha-document",
  path: "C:\\notes\\Alpha.md",
  title: "Alpha.md",
  content: "# Alpha snapshot",
  encoding: "utf-8",
  eol: "lf",
  hadBom: false,
  hadFinalNewline: false,
  readOnly: false,
  revision: { modifiedMs: 1, size: 16, hash: "alpha" },
};

const rangeGeometry = ["getClientRects", "getBoundingClientRect"] as const;
const rangeDescriptors = rangeGeometry.map((name) => Object.getOwnPropertyDescriptor(Range.prototype, name));
beforeAll(() => {
  // JSDOM has no text layout, but editing makes CodeMirror measure selections.
  Object.defineProperty(Range.prototype, "getClientRects", { configurable: true, value: () => [] });
  Object.defineProperty(Range.prototype, "getBoundingClientRect", { configurable: true, value: () => new DOMRect() });
});
afterAll(() => {
  rangeGeometry.forEach((name, index) => {
    const descriptor = rangeDescriptors[index];
    if (descriptor) Object.defineProperty(Range.prototype, name, descriptor);
    else Reflect.deleteProperty(Range.prototype, name);
  });
});

afterEach(() => {
  document.body.replaceChildren();
  document.body.classList.remove("printing");
  closeRequestedHandler = null;
  vi.clearAllMocks();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function resetStartupMocks(): void {
  mocks.confirmDialog.mockReset().mockResolvedValue(true);
  mocks.openDialog.mockReset().mockResolvedValue(null);
  mocks.api.closeDocument.mockReset().mockResolvedValue(undefined);
  mocks.api.saveDocument.mockReset().mockImplementation(async (request) => ({
    status: "saved", path: request.path, revision: alphaDocument.revision,
    content: null, recoveryWarnings: [],
  }));
  mocks.api.saveDocumentAs.mockReset();
  mocks.api.writeAsset.mockReset();
  mocks.api.checkExternalChanges.mockReset().mockResolvedValue([]);
  mocks.api.reloadDocument.mockReset().mockRejectedValue(new Error("Unavailable test document"));
  mocks.api.openWorkspace.mockReset();
  mocks.api.openWorkspaceResource.mockReset().mockResolvedValue(undefined);
  mocks.api.createWorkspaceEntry.mockReset();
  mocks.api.trashWorkspaceEntry.mockReset();
  mocks.api.refreshWorkspace.mockReset();
  mocks.api.renameWorkspaceEntry.mockReset();
  mocks.api.searchWorkspace.mockReset();
  mocks.api.restoreRevision.mockReset();
  mocks.api.listRecovery.mockReset().mockResolvedValue([]);
  mocks.api.loadResource.mockReset().mockRejectedValue(new Error("Unavailable test image"));
  mocks.api.exportHtml.mockReset();
  mocks.api.exportPdf.mockReset();
  mocks.api.updateSession.mockReset().mockImplementation(async (session) => session);
  mocks.api.updateSettings.mockReset().mockImplementation(async (settings) => settings);
  appWindow.destroy.mockReset().mockResolvedValue(undefined);
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  }));
  mocks.api.getSettings.mockResolvedValue(settings);
  mocks.api.getSession.mockResolvedValue({
    schemaVersion: 1,
    workspaceRoot: null,
    tabs: [],
    activePath: null,
  });
  mocks.api.takeStartupTargets.mockResolvedValue([
    { kind: "paths", paths: [alphaDocument.path] },
  ]);
  mocks.api.openPaths.mockReset().mockResolvedValue([alphaDocument]);
  mocks.api.prepareExportSource.mockResolvedValue({ token: "source-1" });
  mocks.prepareExportDocument.mockResolvedValue("<p>Alpha snapshot</p>");
}

async function mountReady(snapshot = alphaDocument): Promise<{ component: ReturnType<typeof mount>; target: HTMLElement }> {
  resetStartupMocks();
  mocks.api.openPaths.mockResolvedValueOnce([snapshot]);
  const target = document.createElement("div");
  document.body.append(target);
  const component = mount(App, { target });
  await vi.waitFor(() => expect(target.textContent).toContain("Alpha.md"));
  return { component, target };
}

function editorView(target: HTMLElement): EditorView {
  const view = EditorView.findFromDOM(target.querySelector(".cm-content")!);
  expect(view).not.toBeNull();
  return view!;
}

function savedResult(content: string | null = null, path = alphaDocument.path) {
  return { status: "saved", path, revision: alphaDocument.revision, content, recoveryWarnings: [] };
}

async function clickMenuCommand(target: HTMLElement, label: string): Promise<void> {
  target.querySelector<HTMLButtonElement>("[data-app-menu-trigger]")?.click();
  await tick();
  const command = Array.from(target.querySelectorAll<HTMLButtonElement>(".app-menu button"))
    .find((button) => button.textContent?.includes(label));
  expect(command).toBeDefined();
  command?.click();
}

describe("reactive command lists", () => {
  const labels = (target: HTMLElement, name = "Quick open") => Array.from(
    target.querySelectorAll(`[aria-label="${name}"] .command-list button span`),
  ).map(item => item.textContent);
  const openPalette = async (commands = false) => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "p", ctrlKey: true, shiftKey: commands }));
    await tick();
  };
  const closePalette = async (target: HTMLElement) => {
    target.querySelector(".palette")!.parentElement!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    await tick();
  };

  it("refreshes an open quick picker after settings load, workspace switches and recent file changes", async () => {
    resetStartupMocks();
    let finishSettings!: (value: unknown) => void;
    mocks.api.getSettings.mockReturnValueOnce(new Promise(resolve => finishSettings = resolve));
    const target = document.createElement("div"); document.body.append(target);
    const component = mount(App, { target });
    const a = { root: "C:\\A", name: "A", entries: [{ name: "workspace-a.md", path: "C:\\A\\workspace-a.md", isDir: false, depth: 0 }] };
    const b = { root: "C:\\B", name: "B", entries: [{ name: "workspace-b.md", path: "C:\\B\\workspace-b.md", isDir: false, depth: 0 }] };
    let finishWorkspace: ((value: typeof a) => void) | undefined;
    try {
      await tick();
      await openPalette();
      expect(labels(target)).toEqual(["Open file"]);
      finishSettings({ ...settings, recentFiles: ["C:\\notes\\recent.md"] });
      await vi.waitFor(() => expect(labels(target)).toContain("recent.md"));
      await vi.waitFor(() => expect(labels(target)).toContain("Alpha.md"));
      await closePalette(target);

      mocks.openDialog.mockResolvedValueOnce(a.root);
      mocks.api.openWorkspace.mockReturnValueOnce(new Promise(resolve => finishWorkspace = resolve));
      await clickMenuCommand(target, "Open folder");
      await vi.waitFor(() => expect(finishWorkspace).toBeTypeOf("function"));
      await openPalette();
      expect(labels(target)).not.toContain("workspace-a.md");
      finishWorkspace!(a);
      await vi.waitFor(() => expect(labels(target)).toContain("workspace-a.md"));
      await closePalette(target);

      mocks.openDialog.mockResolvedValueOnce(b.root);
      mocks.api.openWorkspace.mockResolvedValueOnce(b);
      await clickMenuCommand(target, "Open folder");
      await vi.waitFor(() => expect(target.querySelector(".workspace-name")?.getAttribute("title")).toBe(b.root));
      await openPalette();
      expect(labels(target)).toContain("workspace-b.md");
      expect(labels(target)).not.toContain("workspace-a.md");
      expect(labels(target)).toContain("recent.md");
      await closePalette(target);

      const another = { ...alphaDocument, id: "another-id", title: "another.md", path: "C:\\notes\\another.md" };
      mocks.openDialog.mockResolvedValueOnce(another.path);
      mocks.api.openPaths.mockResolvedValueOnce([another]);
      await clickMenuCommand(target, "Open file");
      await vi.waitFor(() => expect(target.querySelector('[data-tab-id="another-id"]')).not.toBeNull());
      await openPalette();
      await vi.waitFor(() => expect(labels(target)).toContain("another.md"));
    } finally { finishSettings(settings); finishWorkspace?.(a); await unmount(component); }
  });

  it("updates command labels and the quick picker browse label after changing language", async () => {
    const { component, target } = await mountReady();
    try {
      await openPalette(true);
      expect(labels(target, "Command palette")).toContain("New document");
      await closePalette(target);
      await clickMenuCommand(target, "Settings");
      await tick();
      const language = target.querySelectorAll<HTMLSelectElement>('[aria-label="Settings"] select')[1];
      language.value = "zh-CN";
      language.dispatchEvent(new Event("change", { bubbles: true }));
      await tick();
      target.querySelector<HTMLButtonElement>('[aria-label="Settings"] .primary')!.click();
      await vi.waitFor(() => expect(document.documentElement.lang).toBe("zh-CN"));
      await openPalette(true);
      expect(labels(target, "Command palette")).toContain("新建文档");
      expect(labels(target, "Command palette")).not.toContain("New document");
      await closePalette(target);
      await openPalette();
      expect(labels(target)).toContain("打开文件");
      expect(labels(target)).not.toContain("Open file");
    } finally { await unmount(component); }
  });
});

describe("desktop export jobs", () => {
  it("rejects HTML and PDF export during upload, then snapshots the completed image", async () => {
    const { component, target } = await mountReady();
    let finish!: (value: unknown) => void;
    mocks.api.writeAsset.mockReturnValueOnce(new Promise(resolve => finish = resolve));
    mocks.saveDialog.mockReset();
    try {
      const view = editorView(target);
      const paste = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(paste, "clipboardData", { value: { files: [new File(["png"], "image.png", { type: "image/png" })] } });
      view.contentDOM.dispatchEvent(paste);
      await vi.waitFor(() => expect(mocks.api.writeAsset).toHaveBeenCalledOnce());
      for (const label of ["Export HTML", "Export PDF"]) {
        await clickMenuCommand(target, label);
        await vi.waitFor(() => expect(target.textContent).toContain("Images are still uploading"));
      }
      expect(mocks.saveDialog).not.toHaveBeenCalled();
      expect(mocks.api.prepareExportSource).not.toHaveBeenCalled();
      finish({ absolutePath: "C:\\notes\\images\\image.png", markdownPath: "images/image.png" });
      await vi.waitFor(() => expect(view.state.doc.toString()).not.toContain("inkflow-upload://"));
      await vi.waitFor(() => expect(view.state.readOnly).toBe(false));
      mocks.saveDialog.mockResolvedValueOnce("C:\\exports\\Alpha.html");
      mocks.api.prepareExportDestination.mockResolvedValue({ token: "destination-1", path: "C:\\exports\\Alpha.html" });
      mocks.prepareExportDocument.mockImplementationOnce(markdown => renderMarkdown(markdown));
      mocks.api.exportHtml.mockResolvedValue({ action: "saved", path: "C:\\exports\\Alpha.html" });
      await clickMenuCommand(target, "Export HTML");
      await vi.waitFor(() => expect(mocks.api.exportHtml).toHaveBeenCalledOnce());
      expect(mocks.prepareExportDocument.mock.calls[0][0]).toContain("images/image.png");
      expect(mocks.api.exportHtml.mock.calls[0][0].renderedHtml).toContain('src="images/image.png"');
    } finally { await unmount(component); }
  });

  it.each([false, true])("refuses Save As over an open target (dirty: %s)", async dirty => {
    const { component, target } = await mountReady();
    const b = { ...alphaDocument, id: "b-id", path: "C:\\notes\\B.md", title: "B.md", content: "B original" };
    mocks.openDialog.mockResolvedValueOnce(b.path);
    mocks.api.openPaths.mockResolvedValueOnce([b]);
    try {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "o", ctrlKey: true }));
      await vi.waitFor(() => expect(target.querySelectorAll(".document-tab")).toHaveLength(2));
      if (dirty) editorView(target).dispatch({ changes: { from: 0, insert: "B edit\n" } });
      Array.from(target.querySelectorAll<HTMLButtonElement>(".document-tab")).find(tab => tab.title === alphaDocument.path)!.click();
      await tick();
      mocks.saveDialog.mockResolvedValueOnce("c:\\NOTES\\b.MD");
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(target.textContent).toContain("destination is open in another tab"));
      expect(mocks.api.saveDocumentAs).not.toHaveBeenCalled();
      expect(target.querySelectorAll(".document-tab")).toHaveLength(2);
      Array.from(target.querySelectorAll<HTMLButtonElement>(".document-tab")).find(tab => tab.title === b.path)!.click();
      await tick();
      expect(editorView(target).state.doc.toString()).toBe(dirty ? "B edit\nB original" : "B original");
    } finally { await unmount(component); }
  });
  it("keeps an immutable document snapshot while tabs change and blocks duplicates", async () => {
    const { component, target } = await mountReady();
    let chooseDestination: (path: string) => void = () => undefined;
    mocks.saveDialog.mockReturnValueOnce(new Promise<string>((resolve) => {
      chooseDestination = resolve;
    }));
    mocks.api.prepareExportDestination.mockResolvedValue({
      token: "destination-1",
      path: "C:\\exports\\Alpha.html",
    });
    mocks.api.exportHtml.mockResolvedValue({
      action: "saved",
      path: "C:\\exports\\Alpha.html",
    });

    await clickMenuCommand(target, "Export HTML");
    await vi.waitFor(() => expect(mocks.saveDialog).toHaveBeenCalledOnce());

    target.querySelector<HTMLButtonElement>("[data-app-menu-trigger]")?.click();
    await tick();
    const exportButtons = Array.from(target.querySelectorAll<HTMLButtonElement>(".app-menu button"))
      .filter((button) => button.textContent?.includes("Export"));
    expect(exportButtons).toHaveLength(2);
    expect(exportButtons.every((button) => button.disabled)).toBe(true);

    window.dispatchEvent(new KeyboardEvent("keydown", {
      key: "P",
      ctrlKey: true,
      shiftKey: true,
      bubbles: true,
    }));
    await tick();
    const paletteExport = Array.from(target.querySelectorAll<HTMLButtonElement>(".command-list button"))
      .find((button) => button.textContent?.includes("Export HTML"));
    paletteExport?.click();
    await vi.waitFor(() => expect(target.textContent).toContain("already in progress"));

    window.dispatchEvent(new KeyboardEvent("keydown", {
      key: "n",
      ctrlKey: true,
      bubbles: true,
    }));
    chooseDestination("C:\\exports\\Alpha.html");

    await vi.waitFor(() => expect(mocks.api.exportHtml).toHaveBeenCalledOnce());
    expect(mocks.prepareExportDocument).toHaveBeenCalledWith(
      "# Alpha snapshot",
      expect.objectContaining({
        allowRemoteImages: false,
        editorFont: "Snapshot Font",
      }),
    );
    expect(mocks.api.exportHtml).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Alpha.md",
        renderedHtml: "<p>Alpha snapshot</p>",
        outputPath: "C:\\exports\\Alpha.html",
      }),
      "destination-1",
    );
    expect(mocks.api.prepareExportSource).toHaveBeenCalledWith(
      "alpha-document",
      alphaDocument.path,
      null,
    );
    expect(mocks.api.cancelExportSource).toHaveBeenCalledWith("source-1");

    await unmount(component);
  });

  it("loads export resources through the immutable source scope", async () => {
    const { component, target } = await mountReady();
    mocks.saveDialog.mockResolvedValue("C:\\exports\\Alpha.html");
    mocks.api.prepareExportDestination.mockResolvedValue({
      token: "destination-1",
      path: "C:\\exports\\Alpha.html",
    });
    mocks.api.loadExportResource.mockResolvedValue("data:image/png;base64,YQ==");
    mocks.prepareExportDocument.mockImplementationOnce(async (_markdown, options) => {
      const loadResource = (options as { loadResource: (source: string) => Promise<string> })
        .loadResource;
      await loadResource("images/a.png");
      return "<p>Alpha snapshot</p>";
    });
    mocks.api.exportHtml.mockResolvedValue({
      action: "saved",
      path: "C:\\exports\\Alpha.html",
    });

    await clickMenuCommand(target, "Export HTML");
    await vi.waitFor(() => expect(mocks.api.exportHtml).toHaveBeenCalledOnce());

    expect(mocks.api.loadExportResource)
      .toHaveBeenCalledWith("source-1", "images/a.png");
    expect(mocks.api.loadResource).not.toHaveBeenCalled();
    expect(mocks.api.cancelExportSource).toHaveBeenCalledWith("source-1");

    await unmount(component);
  });

  it("fails the export when the resource scope is lost instead of accepting placeholders", async () => {
    const { component, target } = await mountReady();
    mocks.saveDialog.mockResolvedValue("C:\\exports\\Alpha.html");
    mocks.api.prepareExportDestination.mockResolvedValue({
      token: "destination-1",
      path: "C:\\exports\\Alpha.html",
    });
    mocks.api.loadExportResource.mockRejectedValue({
      code: "invalid_export_source",
      message: "The export resource directory changed.",
    });
    mocks.prepareExportDocument.mockImplementationOnce(async (_markdown, options) => {
      const loadResource = (options as { loadResource: (source: string) => Promise<string> })
        .loadResource;
      try {
        await loadResource("images/a.png");
      } catch {
        // The export document pipeline normally converts a missing resource
        // into a placeholder so one bad image does not abort the whole export.
      }
      return "<p>[Missing image: images/a.png]</p>";
    });

    await clickMenuCommand(target, "Export HTML");
    await vi.waitFor(() => expect(target.textContent).toContain("resource directory changed"));

    expect(mocks.api.exportHtml).not.toHaveBeenCalled();
    expect(mocks.api.cancelExportDestination).toHaveBeenCalledWith("destination-1");
    expect(mocks.api.cancelExportSource).toHaveBeenCalledWith("source-1");

    await unmount(component);
  });

  it.each([
    ["revision_conflict", "The destination changed."],
    ["expired_export_token", "The destination confirmation expired."],
  ])("reopens the save dialog after %s and reuses the original snapshot", async (
    errorCode,
    errorMessage,
  ) => {
    const { component, target } = await mountReady();
    mocks.saveDialog
      .mockResolvedValueOnce("C:\\exports\\Alpha.html")
      .mockResolvedValueOnce("C:\\exports\\Alpha-copy.html");
    mocks.api.prepareExportDestination
      .mockResolvedValueOnce({
        token: "destination-1",
        path: "C:\\exports\\Alpha.html",
      })
      .mockResolvedValueOnce({
        token: "destination-2",
        path: "C:\\exports\\Alpha-copy.html",
      });
    mocks.api.exportHtml
      .mockRejectedValueOnce({
        code: errorCode,
        message: errorMessage,
      })
      .mockResolvedValueOnce({
        action: "saved",
        path: "C:\\exports\\Alpha-copy.html",
      });

    await clickMenuCommand(target, "Export HTML");
    await vi.waitFor(() => expect(target.textContent).toContain(errorMessage));
    const reselect = Array.from(target.querySelectorAll<HTMLButtonElement>(".toast button"))
      .find((button) => button.textContent?.includes("Choose again"));
    expect(reselect).toBeDefined();
    reselect?.click();

    await vi.waitFor(() => expect(mocks.api.exportHtml).toHaveBeenCalledTimes(2));
    expect(mocks.saveDialog).toHaveBeenCalledTimes(2);
    expect(mocks.prepareExportDocument).toHaveBeenCalledTimes(2);
    expect(mocks.prepareExportDocument.mock.calls.map(([markdown]) => markdown))
      .toEqual(["# Alpha snapshot", "# Alpha snapshot"]);
    expect(mocks.api.exportHtml.mock.calls[1][1]).toBe("destination-2");
    expect(mocks.api.prepareExportSource).toHaveBeenCalledOnce();
    expect(mocks.api.cancelExportSource).toHaveBeenCalledTimes(1);
    expect(mocks.api.cancelExportSource).toHaveBeenCalledWith("source-1");

    await unmount(component);
  });

  it("does not offer destination reselect when source preparation reports path_changed", async () => {
    const { component, target } = await mountReady();
    mocks.api.prepareExportSource.mockRejectedValueOnce({
      code: "path_changed",
      message: "The recovery resource directory changed.",
    });

    await clickMenuCommand(target, "Export HTML");
    await vi.waitFor(() => expect(target.textContent).toContain("recovery resource directory changed"));

    expect(mocks.saveDialog).not.toHaveBeenCalled();
    expect(target.textContent).not.toContain("Choose again");
    expect(mocks.api.cancelExportSource).not.toHaveBeenCalled();

    await unmount(component);
  });

  it("invalidates a stale reselect action before tracking a new export", async () => {
    const { component, target } = await mountReady();
    mocks.saveDialog
      .mockResolvedValueOnce("C:\\exports\\Alpha.html")
      .mockResolvedValueOnce("C:\\exports\\Alpha-copy.html");
    mocks.api.prepareExportDestination
      .mockResolvedValueOnce({
        token: "destination-1",
        path: "C:\\exports\\Alpha.html",
      })
      .mockResolvedValueOnce({
        token: "destination-2",
        path: "C:\\exports\\Alpha-copy.html",
      });
    let finishSecondExport: (result: { action: string; path: string }) => void = () => undefined;
    mocks.api.exportHtml
      .mockRejectedValueOnce({
        code: "revision_conflict",
        message: "The destination changed.",
      })
      .mockReturnValueOnce(new Promise((resolve) => {
        finishSecondExport = resolve;
      }));

    await clickMenuCommand(target, "Export HTML");
    await vi.waitFor(() => expect(target.textContent).toContain("Choose again"));

    await clickMenuCommand(target, "Export HTML");
    await vi.waitFor(() => expect(mocks.api.exportHtml).toHaveBeenCalledTimes(2));
    expect(target.textContent).not.toContain("Choose again");

    const closePromise = closeRequestedHandler?.({ preventDefault: vi.fn() });
    await tick();
    expect(appWindow.destroy).not.toHaveBeenCalled();

    finishSecondExport({ action: "saved", path: "C:\\exports\\Alpha-copy.html" });
    await closePromise;
    expect(appWindow.destroy).toHaveBeenCalledOnce();

    await unmount(component);
  });

  it("waits for an active export before destroying the window", async () => {
    const { component, target } = await mountReady();
    mocks.saveDialog.mockResolvedValue("C:\\exports\\Alpha.html");
    mocks.api.prepareExportDestination.mockResolvedValue({
      token: "destination-1",
      path: "C:\\exports\\Alpha.html",
    });
    let finishExport: (result: { action: string; path: string }) => void = () => undefined;
    mocks.api.exportHtml.mockReturnValue(new Promise((resolve) => {
      finishExport = resolve;
    }));

    await clickMenuCommand(target, "Export HTML");
    await vi.waitFor(() => expect(mocks.api.exportHtml).toHaveBeenCalledOnce());
    expect(closeRequestedHandler).not.toBeNull();

    const closePromise = closeRequestedHandler?.({ preventDefault: vi.fn() });
    await tick();
    expect(appWindow.destroy).not.toHaveBeenCalled();

    finishExport({ action: "saved", path: "C:\\exports\\Alpha.html" });
    await closePromise;
    expect(appWindow.destroy).toHaveBeenCalledOnce();

    await unmount(component);
  });
});

describe("save recovery warnings", () => {
  it("shows a persistent warning returned by an otherwise successful save", async () => {
    const { component, target } = await mountReady();
    mocks.api.saveDocument.mockResolvedValue({
      status: "saved",
      path: alphaDocument.path,
      revision: alphaDocument.revision,
      content: null,
      recoveryWarnings: [{
        code: "recovery_too_large",
        message: "The document exceeds the recovery limit.",
      }],
    });

    await clickMenuCommand(target, "Save");

    await vi.waitFor(() => expect(target.textContent).toContain(
      "Could not create a recovery snapshot: The document exceeds the recovery limit.",
    ));
    expect(target.querySelector(".toast.error .toast-close")).not.toBeNull();

    await unmount(component);
  });
});

describe("window close safety", () => {
  it("applies an in-flight reload before a cancelled close and saves with the new revision", async () => {
    const intervals = vi.spyOn(globalThis, "setInterval");
    const { component, target } = await mountReady();
    const poll = intervals.mock.calls.find(([, delay]) => delay === 2200)?.[0];
    expect(typeof poll).toBe("function");
    const revision = { hash: "external", size: 19, modifiedMs: 2 };
    const snapshot = { ...alphaDocument, content: "# External revision", revision };
    mocks.api.checkExternalChanges.mockResolvedValueOnce([{
      documentId: alphaDocument.id, path: alphaDocument.path, kind: "modified", revision,
    }]);
    let finishReload!: (value: unknown) => void;
    mocks.api.reloadDocument.mockReturnValueOnce(new Promise(resolve => finishReload = resolve));
    if (typeof poll === "function") poll();
    await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
    // A separate dirty document lets the user cancel without modifying the
    // clean document whose backend revision is already being reloaded.
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
    await tick();
    editorView(target).dispatch({ changes: { from: 0, insert: "unsaved draft" } });
    mocks.confirmDialog.mockResolvedValueOnce(false);
    const pendingClose = closeRequestedHandler?.({ preventDefault: vi.fn() });
    try {
      await tick();
      expect(editorView(target).state.readOnly).toBe(true);
      expect(mocks.confirmDialog).not.toHaveBeenCalled();
      if (typeof poll === "function") poll();
      expect(mocks.api.checkExternalChanges).toHaveBeenCalledOnce();
    } finally {
      finishReload(snapshot);
      await pendingClose;
    }
    await tick();
    expect(mocks.confirmDialog).toHaveBeenCalledOnce();
    expect(appWindow.destroy).not.toHaveBeenCalled();
    expect(target.querySelector(".app-shell")?.hasAttribute("inert")).toBe(false);
    target.querySelector<HTMLElement>(`[data-tab-id="${alphaDocument.id}"]`)?.click();
    await tick();
    const view = editorView(target);
    expect(view.state.doc.toString()).toBe(snapshot.content);
    if (typeof poll === "function") poll();
    await vi.waitFor(() => expect(mocks.api.checkExternalChanges).toHaveBeenCalledTimes(2));
    expect(mocks.api.reloadDocument).toHaveBeenCalledOnce();
    view.dispatch({ changes: { from: view.state.doc.length, insert: "\nnew edit" } });
    await clickMenuCommand(target, "Save");
    await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
    expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({
      content: `${snapshot.content}\nnew edit`, expectedRevision: revision,
    }));
    await unmount(component);
  });

  it("waits for an in-flight reload before destroying an otherwise clean window", async () => {
    const intervals = vi.spyOn(globalThis, "setInterval");
    const { component } = await mountReady();
    const poll = intervals.mock.calls.find(([, delay]) => delay === 2200)?.[0];
    mocks.api.checkExternalChanges.mockResolvedValueOnce([{
      documentId: alphaDocument.id, path: alphaDocument.path, kind: "modified",
      revision: { hash: "external", size: 19, modifiedMs: 2 },
    }]);
    let finishReload!: (value: unknown) => void;
    mocks.api.reloadDocument.mockReturnValueOnce(new Promise(resolve => finishReload = resolve));
    if (typeof poll === "function") poll();
    await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
    const pendingClose = closeRequestedHandler?.({ preventDefault: vi.fn() });
    try {
      await new Promise(resolve => setTimeout(resolve, 0));
      expect(appWindow.destroy).not.toHaveBeenCalled();
    } finally {
      finishReload({ ...alphaDocument, content: "# External revision" });
      await pendingClose;
      await unmount(component);
    }
    expect(appWindow.destroy).toHaveBeenCalledOnce();
  });

  it("freezes editing and new/open commands, shares close requests, and saves after export", async () => {
    const { component, target } = await mountReady();
    mocks.saveDialog.mockResolvedValue("C:\\exports\\Alpha.html");
    mocks.api.prepareExportDestination.mockResolvedValue({ token: "close-export", path: "C:\\exports\\Alpha.html" });
    let finishExport!: (value: unknown) => void;
    mocks.api.exportHtml.mockReturnValueOnce(new Promise(resolve => finishExport = resolve));
    await clickMenuCommand(target, "Export HTML");
    await vi.waitFor(() => expect(mocks.api.exportHtml).toHaveBeenCalledOnce());
    const view = editorView(target);
    view.dispatch({ changes: { from: view.state.doc.length, insert: "\nlatest input" } });
    let finishSave!: (value: unknown) => void;
    mocks.api.saveDocument.mockReturnValueOnce(new Promise(resolve => finishSave = resolve));
    const preventDefault = vi.fn();
    const first = closeRequestedHandler?.({ preventDefault });
    const second = closeRequestedHandler?.({ preventDefault });
    await tick();
    expect(view.state.readOnly).toBe(true);
    expect(insertNewlineAndIndent(view)).toBe(false);
    for (const key of ["n", "o", "s"]) window.dispatchEvent(new KeyboardEvent("keydown", { key, ctrlKey: true, cancelable: true }));
    expect(target.querySelectorAll(".document-tab")).toHaveLength(1);
    expect(mocks.openDialog).not.toHaveBeenCalled();
    expect(appWindow.destroy).not.toHaveBeenCalled();
    finishExport({ action: "saved", path: "C:\\exports\\Alpha.html" });
    await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
    expect(mocks.api.saveDocument.mock.calls[0][0].content).toBe("# Alpha snapshot\nlatest input");
    expect(appWindow.destroy).not.toHaveBeenCalled();
    finishSave(savedResult());
    await Promise.all([first, second]);
    expect(preventDefault).toHaveBeenCalledTimes(2);
    expect(mocks.confirmDialog).toHaveBeenCalledOnce();
    expect(appWindow.destroy).toHaveBeenCalledOnce();
    await unmount(component);
  });

  it.each(["cancel", "save failure", "session failure", "destroy failure"])("unlocks the window after %s", async (failure) => {
    const { component, target } = await mountReady();
    const view = editorView(target);
    view.dispatch({ changes: { from: view.state.doc.length, insert: "\ndirty" } });
    if (failure === "cancel") mocks.confirmDialog.mockResolvedValueOnce(false);
    if (failure === "save failure") mocks.api.saveDocument.mockRejectedValueOnce(new Error("save failed"));
    if (failure === "session failure") mocks.api.updateSession.mockRejectedValueOnce(new Error("session failed"));
    if (failure === "destroy failure") appWindow.destroy.mockRejectedValueOnce(new Error("destroy failed"));
    await closeRequestedHandler?.({ preventDefault: vi.fn() });
    await tick();
    expect(view.state.readOnly).toBe(false);
    expect(target.querySelector(".app-shell")?.hasAttribute("inert")).toBe(false);
    if (failure !== "destroy failure") expect(appWindow.destroy).not.toHaveBeenCalled();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
    await tick();
    expect(target.querySelectorAll(".document-tab")).toHaveLength(2);
    await unmount(component);
  });

  it("waits for an existing save and its newer editor version", async () => {
    const { component, target } = await mountReady();
    let finishSave!: (value: unknown) => void;
    mocks.api.saveDocument.mockReturnValueOnce(new Promise(resolve => finishSave = resolve));
    await clickMenuCommand(target, "Save");
    await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
    const view = editorView(target);
    view.dispatch({ changes: { from: view.state.doc.length, insert: "\nnewer version" } });
    const pendingClose = closeRequestedHandler?.({ preventDefault: vi.fn() });
    await tick();
    expect(appWindow.destroy).not.toHaveBeenCalled();
    finishSave(savedResult());
    await pendingClose;
    expect(mocks.api.saveDocument).toHaveBeenCalledTimes(2);
    expect(mocks.api.saveDocument.mock.calls[1][0].content).toBe("# Alpha snapshot\nnewer version");
    expect(appWindow.destroy).toHaveBeenCalledOnce();
    await unmount(component);
  });

  it("waits for an image paste to finish before saving the closing document", async () => {
    const { component, target } = await mountReady();
    let finishAsset!: (value: unknown) => void;
    mocks.api.writeAsset.mockReturnValueOnce(new Promise(resolve => finishAsset = resolve));
    const view = editorView(target);
    const paste = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(paste, "clipboardData", { value: { files: [new File(["png"], "image.png", { type: "image/png" })] } });
    view.contentDOM.dispatchEvent(paste);
    await vi.waitFor(() => expect(mocks.api.writeAsset).toHaveBeenCalledOnce());
    const pendingClose = closeRequestedHandler?.({ preventDefault: vi.fn() });
    await tick();
    expect(appWindow.destroy).not.toHaveBeenCalled();
    finishAsset({ absolutePath: "C:\\notes\\Alpha.assets\\image.png", markdownPath: "Alpha.assets/image.png" });
    await pendingClose;
    const content = mocks.api.saveDocument.mock.calls.at(-1)?.[0].content;
    expect(content).toContain("![image](Alpha.assets/image.png)");
    expect(content).not.toContain("inkflow-upload://");
    expect(appWindow.destroy).toHaveBeenCalledOnce();
    await unmount(component);
  });
});

describe("concurrent file opening", () => {
  it.each([false, true])("drains an alias open before closing its reused ID (reopen: %s)", async reopen => {
    const { component, target } = await mountReady();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
    await vi.waitFor(() => expect(target.querySelectorAll(".document-tab")).toHaveLength(2));
    target.querySelector<HTMLElement>(`[data-tab-id="${alphaDocument.id}"]`)!.click();
    const alias = "C:\\notes\\sub\\..\\Alpha.md";
    let finishOpen!: (value: unknown) => void;
    let finishClose: (() => void) | undefined;
    let registered = true;
    mocks.api.openPaths.mockClear();
    mocks.api.openPaths
      .mockReturnValueOnce(new Promise(resolve => finishOpen = resolve))
      .mockImplementationOnce(async () => {
        registered = true;
        return [{ ...alphaDocument, id: "reopened-alpha" }];
      });
    mocks.api.closeDocument.mockImplementationOnce(() => new Promise(resolve => {
      finishClose = () => { registered = false; resolve(undefined); };
    }));
    mocks.openDialog.mockResolvedValue(alias);
    try {
      await clickMenuCommand(target, "Open file");
      await vi.waitFor(() => expect(mocks.api.openPaths).toHaveBeenCalledOnce());
      target.querySelector<HTMLButtonElement>(`[data-tab-id="${alphaDocument.id}"] .tab-close`)!.click();
      if (reopen) await clickMenuCommand(target, "Open file");
      await tick();
      expect(mocks.api.closeDocument).not.toHaveBeenCalled();
      expect(mocks.api.openPaths).toHaveBeenCalledOnce();
      finishOpen([alphaDocument]);
      await vi.waitFor(() => expect(finishClose).toBeTypeOf("function"));
      expect(target.querySelector(`[data-tab-id="${alphaDocument.id}"]`)).toBeNull();
      expect(mocks.api.openPaths).toHaveBeenCalledOnce();
      finishClose!();
      if (reopen) {
        await vi.waitFor(() => expect(target.querySelector('[data-tab-id="reopened-alpha"]')).not.toBeNull());
        expect(registered).toBe(true);
        expect(mocks.api.openPaths).toHaveBeenCalledTimes(2);
        const view = editorView(target);
        view.dispatch({ changes: { from: view.state.doc.length, insert: "\nedit after reopen" } });
        window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
        await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalled());
        expect(mocks.api.saveDocument.mock.calls.at(-1)?.[0]).toMatchObject({ id: "reopened-alpha", expectedRevision: alphaDocument.revision });
      } else {
        await vi.waitFor(() => expect(registered).toBe(false));
        expect(target.querySelector(`[data-tab-id="${alphaDocument.id}"]`)).toBeNull();
      }
    } finally { finishOpen([alphaDocument]); finishClose?.(); await unmount(component); }
  });

  it.each(["success", "failure"])("reopens a closed tab before its settings write finishes with %s and still drains on window close", async (outcome) => {
    const { component, target } = await mountReady();
    await vi.waitFor(() => expect(mocks.api.markPerformanceReady).toHaveBeenCalled());
    const path = "C:\\notes\\Beta.md";
    let finishSettings: (() => void) | undefined;
    let pendingClose: void | Promise<void> = undefined;
    mocks.api.updateSettings.mockImplementationOnce((next) => new Promise((resolve, reject) => {
      finishSettings = () => outcome === "success" ? resolve(next) : reject(new Error("settings write failed"));
    }));
    mocks.openDialog.mockResolvedValue(path);
    mocks.api.openPaths.mockClear();
    mocks.api.openPaths
      .mockResolvedValueOnce([{ ...alphaDocument, id: "beta-first", path, title: "Beta.md" }])
      .mockResolvedValueOnce([{ ...alphaDocument, id: "beta-reopened", path, title: "Beta.md" }]);
    try {
      await clickMenuCommand(target, "Open file");
      await vi.waitFor(() => expect(finishSettings).toBeTypeOf("function"));
      target.querySelector<HTMLButtonElement>('[data-tab-id="beta-first"] .tab-close')?.click();
      await vi.waitFor(() => expect(mocks.api.closeDocument).toHaveBeenCalledWith("beta-first"));
      await clickMenuCommand(target, "Open file");
      await vi.waitFor(() => expect(target.querySelector('[data-tab-id="beta-reopened"]')).not.toBeNull());
      expect(mocks.api.openPaths).toHaveBeenCalledTimes(2);
      expect(target.querySelectorAll(".document-tab")).toHaveLength(2);
      expect(mocks.api.closeDocument).toHaveBeenCalledOnce();

      pendingClose = closeRequestedHandler?.({ preventDefault: vi.fn() });
      await tick();
      expect(appWindow.destroy).not.toHaveBeenCalled();
      finishSettings?.();
      await pendingClose;
      expect(appWindow.destroy).toHaveBeenCalledOnce();
      expect(mocks.api.updateSession.mock.calls.at(-1)?.[0].tabs)
        .toEqual([{ path: alphaDocument.path, mode: "live" }, { path, mode: "live" }]);
    } finally {
      finishSettings?.();
      await pendingClose;
      await unmount(component);
    }
  });

  it("keeps a reopened request coalesced when the previous operation finishes", async () => {
    const { component, target } = await mountReady();
    await vi.waitFor(() => expect(mocks.api.markPerformanceReady).toHaveBeenCalled());
    const path = "C:\\notes\\Beta.md";
    const reopened = [{ ...alphaDocument, id: "beta-reopened", path, title: "Beta.md" }];
    let finishSettings: (() => void) | undefined;
    let finishReopen: (() => void) | undefined;
    mocks.api.updateSettings.mockImplementationOnce((next) => new Promise(resolve => {
      finishSettings = () => resolve(next);
    }));
    mocks.openDialog.mockResolvedValue(path);
    mocks.api.openPaths.mockClear();
    mocks.api.openPaths
      .mockResolvedValueOnce([{ ...alphaDocument, id: "beta-first", path, title: "Beta.md" }])
      .mockImplementationOnce(() => new Promise(resolve => { finishReopen = () => resolve(reopened); }));
    try {
      await clickMenuCommand(target, "Open file");
      await vi.waitFor(() => expect(finishSettings).toBeTypeOf("function"));
      target.querySelector<HTMLButtonElement>('[data-tab-id="beta-first"] .tab-close')?.click();
      await vi.waitFor(() => expect(mocks.api.closeDocument).toHaveBeenCalledWith("beta-first"));
      await clickMenuCommand(target, "Open file");
      await vi.waitFor(() => expect(finishReopen).toBeTypeOf("function"));
      finishSettings?.();
      // Drain the first operation's settings continuation and ownership cleanup.
      await new Promise(resolve => setTimeout(resolve, 0));
      await clickMenuCommand(target, "Open file");
      await tick();
      expect(mocks.api.openPaths).toHaveBeenCalledTimes(2);
      finishReopen?.();
      await vi.waitFor(() => expect(target.querySelector('[data-tab-id="beta-reopened"]')).not.toBeNull());
      expect(target.querySelectorAll(".document-tab")).toHaveLength(2);
      expect(mocks.api.closeDocument).toHaveBeenCalledOnce();
    } finally {
      finishSettings?.();
      finishReopen?.();
      await unmount(component);
    }
  });

  it("releases a failed in-flight path so it can be opened again", async () => {
    const { component, target } = await mountReady();
    mocks.api.openPaths.mockClear();
    const path = "C:\\notes\\Beta.md";
    mocks.openDialog.mockResolvedValue(path);
    mocks.api.openPaths.mockRejectedValueOnce(new Error("open failed"));
    await clickMenuCommand(target, "Open file");
    await vi.waitFor(() => expect(target.textContent).toContain("open failed"));
    mocks.api.openPaths.mockResolvedValueOnce([{ ...alphaDocument, id: "beta", path, title: "Beta.md" }]);
    await clickMenuCommand(target, "Open file");
    await vi.waitFor(() => expect(target.querySelectorAll(".document-tab")).toHaveLength(2));
    expect(mocks.api.openPaths).toHaveBeenCalledTimes(2);
    await unmount(component);
  });

  it("coalesces the same path while its open request is pending", async () => {
    const { component, target } = await mountReady();
    mocks.api.openPaths.mockClear();
    const path = "C:\\notes\\Beta.md";
    mocks.openDialog.mockResolvedValue(path);
    let finishOpen!: (value: unknown) => void;
    mocks.api.openPaths.mockReturnValueOnce(new Promise(resolve => finishOpen = resolve));
    await clickMenuCommand(target, "Open file");
    await vi.waitFor(() => expect(mocks.api.openPaths).toHaveBeenCalledOnce());
    await clickMenuCommand(target, "Open file");
    await tick();
    expect(mocks.api.openPaths).toHaveBeenCalledOnce();
    const pendingClose = closeRequestedHandler?.({ preventDefault: vi.fn() });
    await tick();
    expect(appWindow.destroy).not.toHaveBeenCalled();
    finishOpen([{ ...alphaDocument, id: "beta", path, title: "Beta.md" }]);
    await pendingClose;
    expect(target.querySelectorAll(".document-tab")).toHaveLength(2);
    expect(mocks.api.closeDocument).not.toHaveBeenCalled();
    expect(mocks.api.updateSession.mock.calls.at(-1)?.[0].tabs).toContainEqual({ path, mode: "live" });
    await unmount(component);
  });

  it.each([[0, 1], [1, 0]])("deduplicates canonical results returned in order %s, %s", async (first, second) => {
    const { component, target } = await mountReady();
    mocks.api.openPaths.mockClear();
    const path = "C:\\notes\\Beta.md";
    mocks.openDialog.mockResolvedValueOnce("C:\\notes\\sub\\..\\Beta.md").mockResolvedValueOnce(path);
    const finish: Array<(value: unknown) => void> = [];
    mocks.api.openPaths.mockImplementation(() => new Promise(resolve => finish.push(resolve)));
    await clickMenuCommand(target, "Open file");
    await vi.waitFor(() => expect(finish).toHaveLength(1));
    await clickMenuCommand(target, "Open file");
    await vi.waitFor(() => expect(finish).toHaveLength(2));
    finish[first]([{ ...alphaDocument, id: `beta-${first}`, path, title: "Beta.md" }]);
    await vi.waitFor(() => expect(target.querySelectorAll(".document-tab")).toHaveLength(2));
    finish[second]([{ ...alphaDocument, id: `beta-${second}`, path, title: "Beta.md" }]);
    await vi.waitFor(() => expect(mocks.api.closeDocument).toHaveBeenCalledWith(`beta-${second}`));
    expect(target.querySelectorAll(".document-tab")).toHaveLength(2);
    expect(target.querySelector(`[data-tab-id="beta-${first}"]`)).not.toBeNull();
    await unmount(component);
  });
});

describe("image history lifecycle", () => {
  it.each(["undo", "redo"])("migrates a Mermaid image present only in the %s branch", async branch => {
    const base = "# Alpha snapshot\n";
    const diagram = '```mermaid\nflowchart LR\nA@{img: &pic "images/logo.png", label: *pic}\n```';
    const migratedDiagram = '```mermaid\nflowchart LR\nA@{img: &pic "Copy.assets/logo.png", label: &pic "images/logo.png"}\n```';
    const { component, target } = await mountReady({ ...alphaDocument, path: "C:\\A\\note.md", content: branch === "undo" ? base + diagram : base });
    mocks.saveDialog.mockReset().mockResolvedValueOnce("C:\\B\\Copy.md");
    mocks.api.saveDocumentAs.mockImplementationOnce(async request => {
      expect(request.content).toBe(base);
      expect(request.historyImageSources).toEqual(["images/logo.png"]);
      return { ...savedResult(null, "C:\\B\\Copy.md"), assetRewrites: [{ source: "images/logo.png", destination: "Copy.assets/logo.png" }] };
    });
    try {
      const view = editorView(target);
      if (branch === "undo") view.dispatch({ changes: { from: base.length, to: view.state.doc.length } });
      else {
        view.dispatch({ changes: { from: base.length, insert: diagram } });
        expect(undo(view)).toBe(true);
      }
      await tick();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe("C:\\B\\Copy.md"));
      expect(branch === "undo" ? undo(view) : redo(view)).toBe(true);
      expect(view.state.doc.toString()).toBe(base + migratedDiagram);
      await tick();
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0].content).toContain("Copy.assets/logo.png");
    } finally { await unmount(component); }
  });

  it("resolves an upload URL in a redo branch whose image label was edited", async () => {
    const { component, target } = await mountReady();
    let finish!: (result: unknown) => void;
    mocks.api.writeAsset.mockReturnValueOnce(new Promise(resolve => finish = resolve));
    try {
      const view = editorView(target);
      const paste = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(paste, "clipboardData", { value: { files: [new File(["png"], "x.png", { type: "image/png" })] } });
      view.contentDOM.dispatchEvent(paste);
      await vi.waitFor(() => expect(mocks.api.writeAsset).toHaveBeenCalledOnce());
      const label = view.state.doc.toString().indexOf("![x]") + 2;
      view.dispatch({ changes: { from: label, to: label + 1, insert: "custom label" }, annotations: isolateHistory.of("full") });
      expect(undo(view)).toBe(true);
      expect(undo(view)).toBe(true);
      await tick();
      finish({ absolutePath: "C:\\notes\\My images\\x.png", markdownPath: "My images/x.png" });
      await new Promise(resolve => setTimeout(resolve, 0));
      await tick();
      await vi.waitFor(() => expect(view.state.readOnly).toBe(false));
      expect(redo(view)).toBe(true);
      expect(redo(view)).toBe(true);
      expect(view.state.doc.toString()).toContain("![custom label](<My images/x.png>)");
      expect(view.state.doc.toString()).not.toContain("inkflow-upload://");
    } finally { await unmount(component); }
  });

  it.each(imageRewriteFixtures)("installs backend path mappings without losing history: $name", async ({ content, rewritten }) => {
    const { component, target } = await mountReady({ ...alphaDocument, content });
    const before = collectImageDestinations(content);
    const after = collectImageDestinations(rewritten);
    const assetRewrites = before.map((image, index) => ({ source: image.destination, destination: after[index].destination }));
    mocks.saveDialog.mockResolvedValueOnce("C:\\B\\Copy.md");
    mocks.api.saveDocumentAs.mockResolvedValueOnce({ ...savedResult(rewritten + "\nedit", "C:\\B\\Copy.md"), assetRewrites });
    try {
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nedit" } });
      await tick();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe("C:\\B\\Copy.md"));
      expect(view.state.doc.toString()).toBe(rewritten + "\nedit");
      expect(undo(view)).toBe(true);
      expect(view.state.doc.toString()).toBe(rewritten);
      expect(redo(view)).toBe(true);
    } finally { await unmount(component); }
  });

  it.each([false, true])("settles an undone upload before redo (hidden: %s)", async hidden => {
    const { component, target } = await mountReady();
    let finish!: (result: unknown) => void;
    mocks.api.writeAsset.mockReturnValueOnce(new Promise(resolve => finish = resolve));
    try {
      const view = editorView(target);
      const paste = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(paste, "clipboardData", { value: { files: [new File(["png"], "x.png", { type: "image/png" })] } });
      view.contentDOM.dispatchEvent(paste);
      await vi.waitFor(() => expect(mocks.api.writeAsset).toHaveBeenCalledOnce());
      expect(undo(view)).toBe(true);
      await tick();
      if (hidden) {
        window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
        await tick();
      }
      finish({ absolutePath: "C:\\notes\\Alpha.assets\\x.png", markdownPath: "Alpha.assets/x.png" });
      await new Promise(resolve => setTimeout(resolve, 0));
      await tick();
      if (hidden) {
        target.querySelector<HTMLElement>(`[data-tab-id="${alphaDocument.id}"]`)!.click();
        await tick();
      }
      const restored = editorView(target);
      await vi.waitFor(() => expect(restored.state.readOnly).toBe(false));
      expect(redo(restored)).toBe(true);
      expect(restored.state.doc.toString()).toContain("Alpha.assets/x.png");
      expect(restored.state.doc.toString()).not.toContain("inkflow-upload://");
      await tick();
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0].content).toContain("Alpha.assets/x.png");
    } finally { await unmount(component); }
  });

  it("removes a failed upload from the redo branch and allows saving", async () => {
    const { component, target } = await mountReady();
    let fail!: (error: Error) => void;
    mocks.api.writeAsset.mockReturnValueOnce(new Promise((_resolve, reject) => fail = reject));
    try {
      const view = editorView(target);
      const paste = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(paste, "clipboardData", { value: { files: [new File(["png"], "x.png", { type: "image/png" })] } });
      view.contentDOM.dispatchEvent(paste);
      await vi.waitFor(() => expect(mocks.api.writeAsset).toHaveBeenCalledOnce());
      expect(undo(view)).toBe(true);
      await tick();
      fail(new Error("upload failed"));
      await vi.waitFor(() => expect(target.textContent).toContain("upload failed"));
      redo(view);
      expect(view.state.doc.toString()).toBe(alphaDocument.content);
      await tick();
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
    } finally { await unmount(component); }
  });

  it("waits for an undone upload before collecting Save As history resources", async () => {
    const { component, target } = await mountReady();
    let finish!: (result: unknown) => void;
    mocks.api.writeAsset.mockReturnValueOnce(new Promise(resolve => finish = resolve));
    mocks.saveDialog.mockReset().mockResolvedValueOnce("C:\\B\\copy.md");
    mocks.api.saveDocumentAs.mockImplementationOnce(async request => {
      expect(request.content).toBe(alphaDocument.content);
      expect(request.historyImageSources).toContain("Alpha.assets/x.png");
      return { ...savedResult(null, "C:\\B\\copy.md"), assetRewrites: [{ source: "Alpha.assets/x.png", destination: "copy.assets/x.png" }] };
    });
    try {
      const view = editorView(target);
      const paste = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(paste, "clipboardData", { value: { files: [new File(["png"], "x.png", { type: "image/png" })] } });
      view.contentDOM.dispatchEvent(paste);
      await vi.waitFor(() => expect(mocks.api.writeAsset).toHaveBeenCalledOnce());
      expect(undo(view)).toBe(true);
      await tick();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await new Promise(resolve => setTimeout(resolve, 0));
      expect(mocks.saveDialog).not.toHaveBeenCalled();
      expect(mocks.api.saveDocumentAs).not.toHaveBeenCalled();
      finish({ absolutePath: "C:\\notes\\Alpha.assets\\x.png", markdownPath: "Alpha.assets/x.png" });
      await vi.waitFor(() => expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe("C:\\B\\copy.md"));
      expect(redo(view)).toBe(true);
      expect(view.state.doc.toString()).toContain("copy.assets/x.png");
      expect(view.state.doc.toString()).not.toContain("inkflow-upload://");
    } finally { await unmount(component); }
  });

  it.each(["undo", "redo"])("migrates images present only in the %s branch while preview is active", async branch => {
    const base = "# Alpha snapshot\n";
    const original = base + '![x](note.assets/x.png)\n<img srcset="note.assets/x.png 1x">';
    const { component, target } = await mountReady({ ...alphaDocument, path: "C:\\A\\note.md", content: branch === "undo" ? original : base });
    mocks.saveDialog.mockResolvedValueOnce("C:\\B\\Copy name.md");
    mocks.api.saveDocumentAs.mockImplementationOnce(async request => {
      expect(request.content).toBe(base);
      expect(request.historyImageSources).toEqual(["note.assets/x.png"]);
      return { ...savedResult(null, "C:\\B\\Copy name.md"), assetRewrites: [{ source: "note.assets/x.png", destination: "Copy name.assets/x.png" }] };
    });
    try {
      const view = editorView(target);
      if (branch === "undo") view.dispatch({ changes: { from: base.length, to: original.length } });
      else {
        view.dispatch({ changes: { from: base.length, insert: original.slice(base.length) } });
        expect(undo(view)).toBe(true);
      }
      await tick();
      target.querySelector<HTMLButtonElement>('[title="Preview mode"]')!.click();
      await tick();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe("C:\\B\\Copy name.md"));
      target.querySelector<HTMLButtonElement>('[title="Source mode"]')!.click();
      await tick();
      const restored = editorView(target);
      expect(branch === "undo" ? undo(restored) : redo(restored)).toBe(true);
      expect(restored.state.doc.toString()).toBe(base + '![x](<Copy name.assets/x.png>)\n<img srcset="Copy%20name.assets/x.png 1x">');
      await tick();
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0].content).toContain("Copy name.assets/x.png");
    } finally { await unmount(component); }
  });

  it("does not mistake literal upload URL text for an in-flight upload", async () => {
    const content = 'Example: `inkflow-upload://id`';
    const { component, target } = await mountReady({ ...alphaDocument, content });
    try {
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0].content).toBe(content);
    } finally { await unmount(component); }
  });
});

describe("editor history lifecycle", () => {
  it("keeps each tab's undo, redo, and selection across repeated switches", async () => {
    const { component, target } = await mountReady();
    try {
      const first = editorView(target);
      for (const insert of [" first", " second"]) {
        first.dispatch({
          changes: { from: first.state.doc.length, insert },
          annotations: isolateHistory.of("full"),
        });
      }
      expect(undo(first)).toBe(true);
      first.dispatch({ selection: { anchor: 2, head: 7 } });
      await tick();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
      await tick();
      const secondId = target.querySelector<HTMLElement>(".document-tab.active")!.dataset.tabId!;
      const second = editorView(target);
      second.dispatch({ changes: { from: 0, insert: "separate draft" } });
      await tick();
      for (let pass = 0; pass < 2; pass += 1) {
        target.querySelector<HTMLElement>(`[data-tab-id="${alphaDocument.id}"]`)!.click();
        await tick();
        const restored = editorView(target);
        expect(restored.state.selection.main.anchor).toBe(2);
        expect(restored.state.selection.main.head).toBe(7);
        expect(undoDepth(restored.state)).toBe(1);
        expect(redoDepth(restored.state)).toBe(1);
        expect(redo(restored)).toBe(true);
        expect(restored.state.doc.toString()).toBe(`${alphaDocument.content} first second`);
        expect(undo(restored)).toBe(true);
        expect(undo(restored)).toBe(true);
        expect(restored.state.doc.toString()).toBe(alphaDocument.content);
        expect(redo(restored)).toBe(true);
        restored.dispatch({ selection: { anchor: 2, head: 7 } });
        await tick();
        target.querySelector<HTMLElement>(`[data-tab-id="${secondId}"]`)!.click();
        await tick();
        const other = editorView(target);
        expect(other.state.doc.toString()).toBe("separate draft");
        expect(undo(other)).toBe(true);
        expect(other.state.doc.length).toBe(0);
        expect(redo(other)).toBe(true);
        await tick();
      }
    } finally { await unmount(component); }
  });

  it("keeps undo and redo when returning from preview to source mode", async () => {
    const { component, target } = await mountReady();
    try {
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: " edited" } });
      await tick();
      target.querySelector<HTMLButtonElement>('[title="Preview mode"]')!.click();
      await tick();
      expect(target.querySelector(".cm-content")).toBeNull();
      target.querySelector<HTMLButtonElement>('[title="Source mode"]')!.click();
      await tick();
      const restored = editorView(target);
      expect(undo(restored)).toBe(true);
      expect(restored.state.doc.toString()).toBe(alphaDocument.content);
      expect(redo(restored)).toBe(true);
      expect(restored.state.doc.toString()).toBe(`${alphaDocument.content} edited`);
    } finally { await unmount(component); }
  });

  it("rebases both history branches when Save As finishes while another tab is active", async () => {
    const original = "![x](old.png)";
    const migrated = "![x](Copy.assets/old.png)";
    const path = "C:\\B\\Copy.md";
    const { component, target } = await mountReady({ ...alphaDocument, content: original });
    let finish!: (result: unknown) => void;
    mocks.saveDialog.mockResolvedValueOnce(path);
    mocks.api.saveDocumentAs.mockReturnValueOnce(new Promise(resolve => finish = resolve));
    try {
      const view = editorView(target);
      for (const insert of [" first", " second"]) {
        view.dispatch({
          changes: { from: view.state.doc.length, insert },
          annotations: isolateHistory.of("full"),
        });
      }
      expect(undo(view)).toBe(true);
      await tick();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
      expect(view.state.readOnly).toBe(true);
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
      await tick();
      editorView(target).dispatch({ changes: { from: 0, insert: "other tab" } });
      await tick();
      finish(savedResult(`${migrated} first`, path));
      await vi.waitFor(() => expect(target.querySelector(`[data-tab-id="${alphaDocument.id}"]`)?.getAttribute("title")).toBe(path));
      expect(editorView(target).state.doc.toString()).toBe("other tab");
      target.querySelector<HTMLElement>(`[data-tab-id="${alphaDocument.id}"]`)!.click();
      await tick();
      const restored = editorView(target);
      expect(restored.state.readOnly).toBe(false);
      expect(redo(restored)).toBe(true);
      expect(restored.state.doc.toString()).toBe(`${migrated} first second`);
      expect(undo(restored)).toBe(true);
      expect(undo(restored)).toBe(true);
      expect(restored.state.doc.toString()).toBe(migrated);
      expect(redo(restored)).toBe(true);
      await tick();
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({
        path, content: `${migrated} first`,
      }));
    } finally { await unmount(component); }
  });
});

describe("asynchronous history processing", () => {
  it("keeps other tabs editable while collecting images from a long undo history", async () => {
    const content = "![x](x.png)\n\n" + "ordinary paragraph text\n\n".repeat(40000);
    const { component, target } = await mountReady({ ...alphaDocument, content });
    let heartbeats = 0;
    let timer: ReturnType<typeof setInterval> | undefined;
    mocks.saveDialog.mockReset().mockImplementationOnce(async () => {
      timer = setInterval(() => heartbeats++, 0);
      return "C:\\B\\copy.md";
    });
    mocks.api.saveDocumentAs.mockImplementationOnce(async request => {
      expect(heartbeats).toBeGreaterThan(1);
      expect(request.historyImageSources).toEqual(["x.png"]);
      return savedResult(null, request.path);
    });
    try {
      const view = editorView(target);
      for (let index = 0; index < 50; index++) {
        view.dispatch({ changes: { from: view.state.doc.length, insert: " edit" }, annotations: isolateHistory.of("full") });
      }
      await tick();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(heartbeats).toBeGreaterThan(0));
      expect(mocks.api.saveDocumentAs).not.toHaveBeenCalled();
      expect(view.state.readOnly).toBe(true);
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
      await tick();
      const other = editorView(target);
      expect(other.state.readOnly).toBe(false);
      other.dispatch({ changes: { from: 0, insert: "Another tab remains editable" } });
      await tick();
      await vi.waitFor(() => expect(target.querySelector(`[data-tab-id="${alphaDocument.id}"]`)?.getAttribute("title")).toBe("C:\\B\\copy.md"), { timeout: 30000 });
      expect(editorView(target).state.doc.toString()).toBe("Another tab remains editable");
    } finally {
      if (timer) clearInterval(timer);
      await unmount(component);
    }
  }, 45000);

  it("abandons a collected history after the application is unmounted", async () => {
    const { component, target } = await mountReady({ ...alphaDocument, content: "![x](x.png)" });
    let finish!: (images: ReturnType<typeof collectImageDestinations>) => void;
    const parse = vi.spyOn(imageDestinationService, "parseImageDestinations")
      .mockReturnValueOnce(new Promise(resolve => finish = resolve));
    mocks.saveDialog.mockReset().mockResolvedValueOnce("C:\\B\\copy.md");
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
    await vi.waitFor(() => expect(parse).toHaveBeenCalled());
    expect(editorView(target).state.readOnly).toBe(true);
    await unmount(component);
    finish(collectImageDestinations("![x](x.png)"));
    await new Promise(resolve => setTimeout(resolve, 20));
    expect(mocks.api.saveDocumentAs).not.toHaveBeenCalled();
  });

  it("keeps the latest selection and mode when an asynchronous history rewrite finishes", async () => {
    const original = "![x](old.png)";
    const migrated = "![x](copy.assets/old.png)";
    const { component, target } = await mountReady({ ...alphaDocument, content: original });
    let finish!: (images: ReturnType<typeof collectImageDestinations>) => void;
    const parse = vi.spyOn(imageDestinationService, "parseImageDestinations");
    mocks.saveDialog.mockReset().mockResolvedValueOnce("C:\\B\\copy.md");
    mocks.api.saveDocumentAs.mockImplementationOnce(async () => {
      parse.mockReturnValueOnce(new Promise(resolve => finish = resolve));
      return { ...savedResult(migrated, "C:\\B\\copy.md"), assetRewrites: [{ source: "old.png", destination: "copy.assets/old.png" }] };
    });
    try {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(parse).toHaveBeenCalledTimes(2));
      const view = editorView(target);
      view.dispatch({ selection: { anchor: original.length } });
      target.querySelector<HTMLButtonElement>('[title="Source mode"]')!.click();
      await tick();
      finish(collectImageDestinations(original));
      await vi.waitFor(() => expect(view.state.readOnly).toBe(false));
      expect(view.state.doc.toString()).toBe(migrated);
      expect(view.state.selection.main.head).toBe(migrated.length);
      expect(target.querySelector('[title="Source mode"]')?.classList.contains("active")).toBe(true);
    } finally { await unmount(component); }
  });
});

describe("Save As editing lock", () => {
  it("preserves undo and an existing redo branch after migrating images in the locked editor", async () => {
    const original = "![x](old.png)";
    const migrated = "![x](note.assets/old.png)";
    const path = "C:\\B\\note.md";
    const { component, target } = await mountReady({ ...alphaDocument, content: original });
    let finish!: (value: unknown) => void;
    mocks.saveDialog.mockResolvedValueOnce(path);
    mocks.api.saveDocumentAs.mockReturnValueOnce(new Promise(resolve => finish = resolve));
    try {
      const view = editorView(target);
      for (const insert of ["\nfirst edit", "\nsecond edit"]) {
        view.dispatch({
          changes: { from: view.state.doc.length, insert },
          annotations: isolateHistory.of("full"),
        });
      }
      expect(undo(view)).toBe(true);
      await tick();
      expect(undoDepth(view.state)).toBe(1);
      expect(redoDepth(view.state)).toBe(1);
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
      expect(view.state.readOnly).toBe(true);
      expect(undo(view)).toBe(false);
      expect(redo(view)).toBe(false);
      finish(savedResult(`${migrated}\nfirst edit`, path));
      await vi.waitFor(() => expect(view.state.readOnly).toBe(false));

      expect(view.state.doc.toString()).toBe(`${migrated}\nfirst edit`);
      expect(undoDepth(view.state)).toBe(1);
      expect(redoDepth(view.state)).toBe(1);
      expect(redo(view)).toBe(true);
      expect(view.state.doc.toString()).toBe(`${migrated}\nfirst edit\nsecond edit`);
      expect(undo(view)).toBe(true);
      expect(view.state.doc.toString()).toBe(`${migrated}\nfirst edit`);
      expect(undo(view)).toBe(true);
      expect(view.state.doc.toString()).toBe(migrated);
      expect(redo(view)).toBe(true);
      await tick();
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({
        path, content: `${migrated}\nfirst edit`,
      }));
    } finally { await unmount(component); }
  });

  it.each(imageRewriteMerges.slice(0, 8))("locks edits while installing migrated syntax: $name", async ({ saved, rewritten }) => {
    const { component, target } = await mountReady({ ...alphaDocument, content: saved });
    const path = "C:\\export\\Copy.md";
    mocks.saveDialog.mockResolvedValue(path);
    let finishSaveAs!: (value: unknown) => void;
    mocks.api.saveDocumentAs.mockReturnValueOnce(new Promise(resolve => finishSaveAs = resolve));
    try {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
      const view = editorView(target);
      expect(view.state.readOnly).toBe(true);
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: "![new](new.png)" } });
      expect(view.state.doc.toString()).toBe(saved);
      finishSaveAs(savedResult(rewritten, path));
      await vi.waitFor(() => expect(view.state.readOnly).toBe(false));
      expect(view.state.doc.toString()).toBe(rewritten);
      expect(mocks.api.saveDocument).not.toHaveBeenCalled();
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\n\nnew input" } });
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({
        path, content: `${rewritten}\n\nnew input`,
      }));
    } finally {
      await unmount(component);
    }
  });

  it.each(imageRewriteFixtures.slice(0, 5))("preserves migrated resources while editing is locked: $name", async ({ content, rewritten }) => {
    const { component, target } = await mountReady({ ...alphaDocument, content });
    const path = "C:\\export\\Copy.md";
    mocks.saveDialog.mockResolvedValue(path);
    let finishSaveAs!: (value: unknown) => void;
    mocks.api.saveDocumentAs.mockReturnValueOnce(new Promise(resolve => finishSaveAs = resolve));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
    await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
    const view = editorView(target);
    expect(view.state.readOnly).toBe(true);
    view.dispatch({ changes: { from: view.state.doc.length, insert: "\n\nnew input" } });
    expect(view.state.doc.toString()).toBe(content);
    finishSaveAs(savedResult(rewritten, path));
    await vi.waitFor(() => expect(view.state.readOnly).toBe(false));
    expect(view.state.doc.toString()).toBe(rewritten);
    expect(mocks.api.saveDocument).not.toHaveBeenCalled();
    await unmount(component);
  });

  it.each([false, true])("blocks new relative references and pasted images during Save As (target has another image: %s)", async targetHasImage => {
    const { component, target } = await mountReady();
    let finish!: (result: unknown) => void;
    mocks.saveDialog.mockResolvedValueOnce("C:\\B\\note.md");
    mocks.api.saveDocumentAs.mockReturnValueOnce(new Promise(resolve => finish = resolve));
    mocks.api.loadResource.mockImplementation(async (_id, resource) => {
      if (resource === "new.png" && targetHasImage) return "data:image/png;base64,d3Jvbmc=";
      throw new Error("missing image");
    });
    try {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
      const view = editorView(target);
      expect(view.state.readOnly).toBe(true);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\n![new](new.png)" } });
      const paste = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(paste, "clipboardData", { value: { files: [new File(["new image"], "new.png", { type: "image/png" })] } });
      view.contentDOM.dispatchEvent(paste);
      expect(view.state.doc.toString()).toBe(alphaDocument.content);
      expect(mocks.api.writeAsset).not.toHaveBeenCalled();
      finish(savedResult(null, "C:\\B\\note.md"));
      await vi.waitFor(() => expect(view.state.readOnly).toBe(false));
      expect(view.state.doc.toString()).toBe(alphaDocument.content);
      expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe("C:\\B\\note.md");
      expect(mocks.api.saveDocument).not.toHaveBeenCalled();
    } finally { await unmount(component); }
  });

  it("unlocks editing after a failed Save As and keeps the source path", async () => {
    const { component, target } = await mountReady();
    let fail!: (error: Error) => void;
    mocks.saveDialog.mockResolvedValueOnce("C:\\B\\note.md");
    mocks.api.saveDocumentAs.mockReturnValueOnce(new Promise((_resolve, reject) => fail = reject));
    try {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
      const view = editorView(target);
      expect(view.state.readOnly).toBe(true);
      fail(new Error("Save As failed"));
      await vi.waitFor(() => expect(view.state.readOnly).toBe(false));
      expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe(alphaDocument.path);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\ncontinued" } });
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0].content).toContain("continued");
      expect(mocks.api.saveDocument.mock.calls[0][0].path).toBe(alphaDocument.path);
    } finally { await unmount(component); }
  });

  it("waits for uploads started in the Save As dialog and saves their completed references", async () => {
    const { component, target } = await mountReady();
    let select!: (path: string) => void;
    let finishAsset!: (result: unknown) => void;
    mocks.saveDialog.mockReturnValueOnce(new Promise(resolve => select = resolve));
    mocks.api.writeAsset.mockReturnValueOnce(new Promise(resolve => finishAsset = resolve));
    mocks.api.saveDocumentAs.mockImplementationOnce(async request => {
      expect(request.content).toContain("Alpha.assets/new.png");
      return {
        ...savedResult(request.content.replace("Alpha.assets/new.png", "note.assets/new.png"), "C:\\B\\note.md"),
        assetRewrites: [{ source: "Alpha.assets/new.png", destination: "note.assets/new.png" }],
      };
    });
    try {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.saveDialog).toHaveBeenCalled());
      const view = editorView(target);
      const paste = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(paste, "clipboardData", { value: { files: [new File(["png"], "new.png", { type: "image/png" })] } });
      view.contentDOM.dispatchEvent(paste);
      await vi.waitFor(() => expect(mocks.api.writeAsset).toHaveBeenCalledOnce());
      select("C:\\B\\note.md");
      await new Promise(resolve => setTimeout(resolve, 0));
      expect(mocks.api.saveDocumentAs).not.toHaveBeenCalled();
      expect(view.state.readOnly).toBe(false);
      finishAsset({ absolutePath: "C:\\notes\\Alpha.assets\\new.png", markdownPath: "Alpha.assets/new.png" });
      await vi.waitFor(() => expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe("C:\\B\\note.md"));
      expect(view.state.doc.toString()).toContain("note.assets/new.png");
      expect(view.state.doc.toString()).not.toContain("inkflow-upload://");
    } finally { await unmount(component); }
  });
});

describe("restored documents and read-only copies", () => {
  it("restores editable text and shows persistent warnings when images are unavailable", async () => {
    const { component, target } = await mountReady();
    const content = "Important recovered text\n\n![image](large.png)";
    mocks.api.listRecovery.mockResolvedValueOnce([{
      id: "checkpoint", documentId: "original", path: "C:\\notes\\Lost.md", title: "Lost.md",
      createdAt: "2026-09-06T00:00:00Z", kind: "history", size: content.length,
    }]);
    mocks.api.restoreRevision.mockResolvedValueOnce({
      document: { ...alphaDocument, id: "recovered-text", path: null, title: "Lost.md", content, revision: null },
      warnings: [{ code: "resource_too_large", message: "large.png: Image exceeds 50 MiB." }],
    });
    try {
      await clickMenuCommand(target, "Recovery history");
      await vi.waitFor(() => expect(target.querySelector('.entries button[title="Restore"]')).not.toBeNull());
      target.querySelector<HTMLButtonElement>('.entries button[title="Restore"]')!.click();
      await vi.waitFor(() => expect(target.querySelector('[data-tab-id="recovered-text"]')).not.toBeNull());
      expect(editorView(target).state.doc.toString()).toBe(content);
      expect(editorView(target).state.readOnly).toBe(false);
      expect(target.querySelector('[aria-label="Recovery history"]')).toBeNull();
      expect(target.textContent).toContain("Text restored, but 1 image resource(s) could not be restored");
      expect(target.textContent).toContain("large.png: Image exceeds 50 MiB.");
      expect(target.querySelector(".toast.error .toast-close")).not.toBeNull();
      editorView(target).dispatch({ changes: { from: 0, to: content.length, insert: "Recovered text without the broken image" } });
      expect(editorView(target).state.doc.toString()).toContain("without the broken image");
    } finally { await unmount(component); }
  });
  it("keeps the restored backend ID for preview and first save", async () => {
    const { component, target } = await mountReady();
    const entry = { id: "checkpoint", documentId: "old-id", path: null, title: "Draft.md", createdAt: "2026-09-06T00:00:00Z", kind: "draft", size: 40 };
    const content = "# Draft\n\n![image](inkflow-asset://image-restored.png)";
    mocks.api.listRecovery.mockResolvedValueOnce([entry]);
    mocks.api.restoreRevision.mockResolvedValueOnce({ document: { ...alphaDocument, id: "restored-id", path: null, title: "Draft.md", content, revision: null }, warnings: [] });
    mocks.api.loadResource.mockResolvedValue("data:image/png;base64,aW1hZ2U=");
    try {
      await clickMenuCommand(target, "Recovery history");
      await vi.waitFor(() => expect(target.querySelector('.entries button[title="Restore"]')).not.toBeNull());
      target.querySelector<HTMLButtonElement>('.entries button[title="Restore"]')!.click();
      await vi.waitFor(() => expect(target.querySelector('[data-tab-id="restored-id"]')).not.toBeNull());
      await vi.waitFor(() => expect(mocks.api.loadResource).toHaveBeenCalledWith("restored-id", "inkflow-asset://image-restored.png"));
      mocks.saveDialog.mockResolvedValue("C:\\export\\Recovered.md");
      mocks.api.saveDocumentAs.mockResolvedValueOnce(savedResult("![image](Recovered.assets/image-restored.png)", "C:\\export\\Recovered.md"));
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocumentAs.mock.calls[0][0]).toEqual(expect.objectContaining({ id: "restored-id", content }));
      await vi.waitFor(() => expect(editorView(target).state.doc.toString()).not.toContain("inkflow-asset://"));
    } finally { await unmount(component); }
  });

  it("saves a read-only source as a writable copy and allows further edits", async () => {
    const { component, target } = await mountReady({ ...alphaDocument, readOnly: true });
    try {
      mocks.saveDialog.mockReset().mockResolvedValue("C:\\export\\Copy.md");
      mocks.api.saveDocumentAs.mockResolvedValueOnce(savedResult(null, "C:\\export\\Copy.md"));
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
      await tick();
      expect(mocks.saveDialog).not.toHaveBeenCalled();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
      await vi.waitFor(() => expect(editorView(target).state.readOnly).toBe(false));
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nnew input" } });
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({ path: "C:\\export\\Copy.md", content: "# Alpha snapshot\nnew input" }));
    } finally { await unmount(component); }
  });

  it("rejects saving a read-only source back to its original path", async () => {
    const { component, target } = await mountReady({ ...alphaDocument, readOnly: true });
    try {
      mocks.saveDialog.mockResolvedValue(alphaDocument.path.toUpperCase());
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(target.textContent).toContain("Choose another save path"));
      expect(mocks.api.saveDocumentAs).not.toHaveBeenCalled();
      expect(editorView(target).state.readOnly).toBe(true);
    } finally { await unmount(component); }
  });
});

describe("external polling response races", () => {
  it("does not reinstall a tab closed while failed-save reconciliation is pending", async () => {
    const { component, target } = await mountReady();
    let finishReconciliation!: (value: unknown) => void;
    mocks.api.reloadDocument.mockReturnValueOnce(new Promise(resolve => finishReconciliation = resolve));
    mocks.api.saveDocumentAs.mockRejectedValueOnce(new Error("Save As failed"));
    mocks.saveDialog.mockResolvedValue("C:\\missing\\Copy.md");
    try {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "n", ctrlKey: true }));
      await tick();
      target.querySelector<HTMLButtonElement>('[data-tab-id="alpha-document"] .tab-close')!.click();
      await vi.waitFor(() => expect(target.querySelector('[data-tab-id="alpha-document"]')).toBeNull());
      finishReconciliation({ ...alphaDocument, content: "new external text" });
      await vi.waitFor(() => expect(mocks.api.closeDocument).toHaveBeenCalledWith(alphaDocument.id));
      expect(target.querySelector('[data-tab-id="alpha-document"]')).toBeNull();
      expect(editorView(target).state.doc.toString()).toBe("");
    } finally { await unmount(component); }
  });

  it.each(["before", "after"] as const)("resynchronizes a failed Save As when the invalidated reload arrives %s the failure", async (delivery) => {
    const intervals = vi.spyOn(globalThis, "setInterval");
    const { component, target } = await mountReady();
    const poll = intervals.mock.calls.find(([, delay]) => delay === 2200)?.[0];
    const revision = { hash: "external", size: 17, modifiedMs: 2 };
    const snapshot = { ...alphaDocument, content: "new external text", revision };
    let finishOldReload!: (value: unknown) => void;
    let failSave!: (error: Error) => void;
    mocks.api.checkExternalChanges.mockResolvedValueOnce([{
      documentId: alphaDocument.id, path: alphaDocument.path, kind: "modified", revision,
    }]);
    mocks.api.reloadDocument
      .mockReturnValueOnce(new Promise(resolve => finishOldReload = resolve))
      .mockResolvedValueOnce(snapshot);
    mocks.api.saveDocumentAs.mockReturnValueOnce(new Promise((_resolve, reject) => failSave = reject));
    mocks.saveDialog.mockResolvedValue("C:\\missing\\Copy.md");
    try {
      if (typeof poll === "function") poll();
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
      if (delivery === "before") {
        finishOldReload(snapshot);
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(editorView(target).state.doc.toString()).toBe(alphaDocument.content);
      }
      failSave(new Error("Save As destination disappeared"));
      await vi.waitFor(() => expect(target.textContent).toContain("Save As destination disappeared"));
      if (delivery === "after") {
        expect(mocks.api.reloadDocument).toHaveBeenCalledOnce();
        finishOldReload(snapshot);
      }
      await vi.waitFor(() => expect(editorView(target).state.doc.toString()).toBe(snapshot.content));
      expect(mocks.api.reloadDocument).toHaveBeenCalledTimes(2);
      expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe(alphaDocument.path);
      expect(target.querySelector(".conflict-banner")).toBeNull();
      expect(target.textContent).toContain("Save As destination disappeared");
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nnext edit" } });
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({
        path: alphaDocument.path, expectedRevision: revision, content: `${snapshot.content}\nnext edit`,
      }));
    } finally { await unmount(component); }
  });

  it("preserves edits made during failed-save reconciliation and reports the external version", async () => {
    const intervals = vi.spyOn(globalThis, "setInterval");
    const { component, target } = await mountReady();
    const poll = intervals.mock.calls.find(([, delay]) => delay === 2200)?.[0];
    const revision = { hash: "external", size: 17, modifiedMs: 2 };
    const snapshot = { ...alphaDocument, content: "new external text", revision };
    let finishOldReload!: (value: unknown) => void;
    let finishReconciliation!: (value: unknown) => void;
    mocks.api.checkExternalChanges.mockResolvedValueOnce([{
      documentId: alphaDocument.id, path: alphaDocument.path, kind: "modified", revision,
    }]);
    mocks.api.reloadDocument
      .mockReturnValueOnce(new Promise(resolve => finishOldReload = resolve))
      .mockReturnValueOnce(new Promise(resolve => finishReconciliation = resolve));
    mocks.api.saveDocumentAs.mockRejectedValueOnce(new Error("Save As failed"));
    mocks.saveDialog.mockResolvedValue("C:\\missing\\Copy.md");
    try {
      if (typeof poll === "function") poll();
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(target.textContent).toContain("Save As failed"));
      finishOldReload(snapshot);
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledTimes(2));
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nlocal edit" } });
      finishReconciliation(snapshot);
      await vi.waitFor(() => expect(target.querySelector(".conflict-banner")).not.toBeNull());
      expect(view.state.doc.toString()).toBe(`${alphaDocument.content}\nlocal edit`);
      expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe(alphaDocument.path);
      expect(target.textContent).toContain("Save As failed");
    } finally { await unmount(component); }
  });

  it("lets a newer reload supersede an earlier request before either response arrives", async () => {
    const { component, target } = await mountReady();
    const finish: Array<(value: unknown) => void> = [];
    mocks.api.reloadDocument.mockImplementation(() => new Promise(resolve => finish.push(resolve)));
    try {
      mocks.api.saveDocument.mockResolvedValueOnce({
        status: "conflict", path: alphaDocument.path, diskRevision: { ...alphaDocument.revision, modifiedMs: 2 },
      });
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
      await vi.waitFor(() => expect(target.querySelector(".conflict-banner")).not.toBeNull());
      const reload = Array.from(target.querySelectorAll<HTMLButtonElement>(".conflict-banner button"))
        .find(button => button.textContent === "Reload")!;
      reload.click();
      await vi.waitFor(() => expect(finish).toHaveLength(1));
      reload.click();
      await vi.waitFor(() => expect(finish).toHaveLength(2));
      finish[0]({ ...alphaDocument, content: "obsolete disk text" });
      await new Promise(resolve => setTimeout(resolve, 0));
      expect(editorView(target).state.doc.toString()).toBe(alphaDocument.content);
      finish[1]({ ...alphaDocument, content: "latest disk text", revision: { hash: "latest", size: 16, modifiedMs: 3 } });
      await vi.waitFor(() => expect(editorView(target).state.doc.toString()).toBe("latest disk text"));
      expect(target.querySelector(".conflict-banner")).toBeNull();
    } finally { await unmount(component); }
  });

  it("still applies an in-flight reload when the Save As dialog is cancelled", async () => {
    const intervals = vi.spyOn(globalThis, "setInterval");
    const { component, target } = await mountReady();
    const poll = intervals.mock.calls.find(([, delay]) => delay === 2200)?.[0];
    let finishReload!: (value: unknown) => void;
    let finishDialog!: (value: null) => void;
    mocks.api.reloadDocument.mockReturnValueOnce(new Promise(resolve => finishReload = resolve));
    mocks.saveDialog.mockReset().mockReturnValueOnce(new Promise(resolve => finishDialog = resolve));
    const revision = { hash: "external", size: 18, modifiedMs: 2 };
    mocks.api.checkExternalChanges.mockResolvedValueOnce([{
      documentId: alphaDocument.id, path: alphaDocument.path, kind: "modified", revision,
    }]);
    try {
      if (typeof poll === "function") poll();
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.saveDialog).toHaveBeenCalledOnce());
      finishDialog(null);
      await new Promise(resolve => setTimeout(resolve, 0));
      finishReload({ ...alphaDocument, content: "new disk text", revision });
      await vi.waitFor(() => expect(editorView(target).state.doc.toString()).toBe("new disk text"));
      expect(mocks.api.saveDocumentAs).not.toHaveBeenCalled();
    } finally { await unmount(component); }
  });

  it.each([
    ["manual", true], ["poll", true], ["comparison", true],
    ["manual", false], ["poll", false], ["comparison", false],
  ] as const)("discards a late %s reload after saving (Save As: %s)", async (entry, saveAs) => {
    const intervals = vi.spyOn(globalThis, "setInterval");
    const { component, target } = await mountReady();
    const poll = intervals.mock.calls.find(([, delay]) => delay === 2200)?.[0];
    const oldRevision = { ...alphaDocument.revision, modifiedMs: 2 };
    const savedRevision = { ...alphaDocument.revision, modifiedMs: 3 };
    let finishReload!: (value: unknown) => void;
    mocks.api.reloadDocument.mockReturnValueOnce(new Promise(resolve => finishReload = resolve));
    try {
      if (entry === "poll") {
        mocks.api.checkExternalChanges.mockResolvedValueOnce([{
          documentId: alphaDocument.id, path: alphaDocument.path, kind: "modified", revision: oldRevision,
        }]);
        expect(typeof poll).toBe("function");
        if (typeof poll === "function") poll();
      } else {
        mocks.api.saveDocument.mockResolvedValueOnce({ status: "conflict", path: alphaDocument.path, diskRevision: oldRevision });
        window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
        await vi.waitFor(() => expect(target.querySelector(".conflict-banner")).not.toBeNull());
        const button = Array.from(target.querySelectorAll<HTMLButtonElement>(".conflict-banner button"))
          .find(button => button.textContent === (entry === "manual" ? "Reload" : "Compare"));
        expect(button).toBeDefined();
        button!.click();
      }
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
      const path = saveAs ? "C:\\export\\Copy.md" : alphaDocument.path;
      mocks.saveDialog.mockResolvedValue(path);
      const result = { ...savedResult(null, path), revision: savedRevision };
      mocks.api.saveDocumentAs.mockResolvedValueOnce(result);
      mocks.api.saveDocument.mockClear().mockResolvedValueOnce(result);
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: saveAs }));
      await vi.waitFor(() => expect(target.querySelector(".conflict-banner")).toBeNull());
      await vi.waitFor(() => expect(saveAs ? mocks.api.saveDocumentAs : mocks.api.saveDocument).toHaveBeenCalledOnce());
      await tick();
      finishReload({ ...alphaDocument, revision: oldRevision });
      await new Promise(resolve => setTimeout(resolve, 0));
      await tick();
      expect(target.querySelector(".conflict-dialog")).toBeNull();
      expect(target.querySelector('.document-tab.active')?.getAttribute("title")).toBe(path);
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nnext edit" } });
      mocks.api.saveDocument.mockClear();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({ path, expectedRevision: savedRevision }));
    } finally { await unmount(component); }
  });

  it("keeps a genuine external conflict when only the editor text changed", async () => {
    const intervals = vi.spyOn(globalThis, "setInterval");
    const { component, target } = await mountReady();
    const poll = intervals.mock.calls.find(([, delay]) => delay === 2200)?.[0];
    let finishPoll!: (changes: ExternalChange[]) => void;
    mocks.api.checkExternalChanges.mockReturnValueOnce(new Promise(resolve => finishPoll = resolve));
    try {
      if (typeof poll === "function") poll();
      await vi.waitFor(() => expect(mocks.api.checkExternalChanges).toHaveBeenCalledOnce());
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nlocal input" } });
      finishPoll([{
        documentId: alphaDocument.id, path: alphaDocument.path, kind: "modified",
        revision: { hash: "external", modifiedMs: 2, size: 30 },
      }]);
      await vi.waitFor(() => expect(target.textContent).toContain("This file changed outside InkFlow"));
      expect(view.state.doc.toString()).toBe("# Alpha snapshot\nlocal input");
      expect(mocks.api.reloadDocument).not.toHaveBeenCalled();
    } finally {
      finishPoll?.([]);
      await unmount(component);
    }
  });
  it.each(["modified", "deleted"] as const)("discards a stale %s response after a save and keeps autosave working", async (kind) => {
    const intervals = vi.spyOn(globalThis, "setInterval");
    const { component, target } = await mountReady();
    const poll = intervals.mock.calls.find(([, delay]) => delay === 2200)?.[0];
    let finishPoll!: (changes: ExternalChange[]) => void;
    mocks.api.checkExternalChanges.mockReturnValueOnce(new Promise(resolve => finishPoll = resolve));
    // A fresh revision object from IPC also marks a successful no-op save.
    mocks.api.saveDocument.mockResolvedValueOnce({ ...savedResult(), revision: { ...alphaDocument.revision } });
    try {
      expect(typeof poll).toBe("function");
      if (typeof poll === "function") poll();
      await vi.waitFor(() => expect(mocks.api.checkExternalChanges).toHaveBeenCalledOnce());
      await clickMenuCommand(target, "Save");
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      await tick();
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nnew input" } });
      finishPoll([{
        documentId: alphaDocument.id, path: alphaDocument.path, kind,
        revision: kind === "deleted" ? null : { hash: "outdated", modifiedMs: 0, size: 1 },
      }]);
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledTimes(2), { timeout: 2500 });
      expect(mocks.api.saveDocument.mock.calls[1][0]).toEqual(expect.objectContaining({ content: "# Alpha snapshot\nnew input" }));
      expect(mocks.api.reloadDocument).not.toHaveBeenCalled();
    } finally {
      finishPoll?.([]);
      await unmount(component);
    }
  });
});

describe("workspace resources and image insertion", () => {
  it.each(["bmp", "pdf"])("opens a workspace %s externally without creating an editable tab", async extension => {
    const { component, target } = await mountReady();
    const entry = { name: `resource.${extension}`, path: `C:\\notes\\resource.${extension}`, isDir: false, depth: 0 };
    try {
      mocks.api.openWorkspace.mockResolvedValueOnce({ root: "C:\\notes", name: "notes", entries: [entry] });
      mocks.openDialog.mockResolvedValueOnce("C:\\notes");
      await clickMenuCommand(target, "Open folder");
      await vi.waitFor(() => expect(target.querySelector(".file-row .file-main")).not.toBeNull());
      target.querySelector<HTMLButtonElement>(".file-row .file-main")!.click();
      await vi.waitFor(() => expect(mocks.api.openWorkspaceResource).toHaveBeenCalledWith(entry.path));
      expect(mocks.api.openPaths).toHaveBeenCalledTimes(1);
      expect(target.querySelectorAll(".document-tab")).toHaveLength(1);
      expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe(alphaDocument.path);
      expect(mocks.api.saveDocument).not.toHaveBeenCalled();
    } finally { await unmount(component); }
  });

  it.each(["photo].png", "photo[.png"])("pastes %s as a real Markdown image", async name => {
    const { component, target } = await mountReady();
    try {
      mocks.api.writeAsset.mockResolvedValueOnce({ absolutePath: "C:\\notes\\Copy).assets\\image.png", markdownPath: "Copy).assets/image.png" });
      const view = editorView(target);
      const paste = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(paste, "clipboardData", { value: { files: [new File(["png"], name, { type: "image/png" })] } });
      view.contentDOM.dispatchEvent(paste);
      await vi.waitFor(() => expect(view.state.doc.toString()).toContain("Copy).assets/image.png"));
      const container = document.createElement("div");
      container.innerHTML = await renderMarkdown(view.state.doc.toString());
      const image = container.querySelector("img");
      expect(image?.getAttribute("src")).toBe("Copy).assets/image.png");
      expect(image?.getAttribute("alt")).toBe(name.slice(0, -4));
    } finally { await unmount(component); }
  });
});

describe("workspace mutation response races", () => {
  it.each(["folder", "file", "rename", "delete", "refresh"].flatMap(kind => [false, true].map(returnToA => ({ kind, returnToA }))))(
    "discards a late $kind snapshot after switching workspaces (return to A: $returnToA)",
    async ({ kind, returnToA }) => {
      const { component, target } = await mountReady();
      const entry = { name: "Alpha.md", path: alphaDocument.path, isDir: false, depth: 0 };
      const moved = { ...alphaDocument, path: "C:\\notes\\Late.md", title: "Late.md" };
      const a = { root: "C:\\notes", name: "notes", entries: [entry] };
      const b = { root: "C:\\B", name: "B", entries: [{ ...entry, name: "Fresh.md", path: "C:\\B\\Fresh.md" }] };
      const freshA = { ...a, entries: [{ ...entry, name: "Fresh.md", path: "C:\\notes\\Fresh.md" }] };
      let finish!: (snapshot: typeof a) => void;
      const operation = kind === "folder" || kind === "file" ? mocks.api.createWorkspaceEntry
        : kind === "rename" ? mocks.api.renameWorkspaceEntry
        : kind === "delete" ? mocks.api.trashWorkspaceEntry : mocks.api.refreshWorkspace;
      operation.mockReturnValueOnce(new Promise(resolve => finish = resolve));
      mocks.api.reloadDocument.mockResolvedValue(moved);
      try {
        mocks.api.openWorkspace.mockResolvedValueOnce(a).mockResolvedValueOnce(b).mockResolvedValueOnce(freshA);
        mocks.openDialog.mockResolvedValueOnce(a.root);
        await clickMenuCommand(target, "Open folder");
        await vi.waitFor(() => expect(target.querySelector(".file-row .file-main")).not.toBeNull());
        vi.spyOn(window, "prompt").mockReturnValue("Late.md");
        if (kind === "folder" || kind === "file") {
          target.querySelector<HTMLButtonElement>(`[title="${kind === "folder" ? "New folder" : "New document"}"]`)!.click();
        } else if (kind === "rename") {
          target.querySelector(".file-row .file-main")!.dispatchEvent(new KeyboardEvent("keydown", { key: "F2", bubbles: true }));
        } else if (kind === "delete") {
          target.querySelector<HTMLButtonElement>(".row-menu")!.click();
          await tick();
          target.querySelector<HTMLButtonElement>(".entry-menu .danger")!.click();
        } else {
          target.querySelector<HTMLButtonElement>('[title="Refresh"]')!.click();
        }
        await vi.waitFor(() => expect(operation).toHaveBeenCalledOnce());
        mocks.openDialog.mockResolvedValueOnce(b.root);
        await clickMenuCommand(target, "Open folder");
        await vi.waitFor(() => expect(target.querySelector(".workspace-name")?.getAttribute("title")).toBe(b.root));
        if (returnToA) {
          mocks.openDialog.mockResolvedValueOnce(a.root);
          await clickMenuCommand(target, "Open folder");
          await vi.waitFor(() => expect(target.querySelector(".workspace-name")?.getAttribute("title")).toBe(a.root));
        }
        finish({ ...a, entries: kind === "delete" ? [] : [{ ...entry, name: "Late.md", path: moved.path }] });
        await new Promise(resolve => setTimeout(resolve, 0));
        await tick();
        expect(target.querySelector(".workspace-name")?.getAttribute("title")).toBe(returnToA ? a.root : b.root);
        expect(target.querySelector(".file-list")?.textContent).toContain("Fresh.md");
        expect(target.querySelector(".file-list")?.textContent).not.toContain("Late.md");
        expect(mocks.api.openPaths).toHaveBeenCalledTimes(1);
        if (kind === "rename") {
          await vi.waitFor(() => expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe(moved.path));
          const view = editorView(target);
          view.dispatch({ changes: { from: view.state.doc.length, insert: "\nnew edit" } });
          await clickMenuCommand(target, "Save");
          await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalled());
          expect(mocks.api.saveDocument.mock.calls.at(-1)?.[0].path).toBe(moved.path);
        } else if (kind === "delete") {
          expect(mocks.api.closeDocument).toHaveBeenCalledWith(alphaDocument.id);
          expect(target.querySelector('[data-tab-id="alpha-document"]')).toBeNull();
        }
      } finally { await unmount(component); }
    },
  );
});

describe("workspace rename response races", () => {
  it("keeps a newer rename in control when the previous reconciliation arrives late", async () => {
    const { component, target } = await mountReady();
    const entry = { name: "Alpha.md", path: alphaDocument.path, isDir: false, depth: 0 };
    const moved = { ...alphaDocument, path: "C:\\notes\\Renamed.md", title: "Renamed.md", revision: { ...alphaDocument.revision, modifiedMs: 2 } };
    let finishOldReconciliation!: (value: unknown) => void;
    let finishNewReconciliation!: (value: unknown) => void;
    try {
      mocks.api.openWorkspace.mockResolvedValueOnce({ root: "C:\\notes", name: "notes", entries: [entry] });
      mocks.openDialog.mockResolvedValueOnce("C:\\notes");
      await clickMenuCommand(target, "Open folder");
      await vi.waitFor(() => expect(target.querySelector(".file-row .file-main")).not.toBeNull());
      vi.spyOn(window, "prompt").mockReturnValueOnce("Taken.md").mockReturnValueOnce("Renamed.md");
      mocks.api.renameWorkspaceEntry
        .mockRejectedValueOnce(new Error("A file with this name already exists."))
        .mockResolvedValueOnce({ root: "C:\\notes", name: "notes", entries: [{ ...entry, name: moved.title, path: moved.path }] });
      mocks.api.reloadDocument
        .mockReturnValueOnce(new Promise(resolve => finishOldReconciliation = resolve))
        .mockReturnValueOnce(new Promise(resolve => finishNewReconciliation = resolve));
      target.querySelector(".file-row .file-main")!.dispatchEvent(new KeyboardEvent("keydown", { key: "F2", bubbles: true }));
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
      target.querySelector(".file-row .file-main")!.dispatchEvent(new KeyboardEvent("keydown", { key: "F2", bubbles: true }));
      await vi.waitFor(() => expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe(moved.path));
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nlocal edit" } });
      finishOldReconciliation(alphaDocument);
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledTimes(2));
      await new Promise(resolve => setTimeout(resolve, settings.autosaveDelayMs + 100));
      expect(mocks.api.saveDocument).not.toHaveBeenCalled();
      expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe(moved.path);
      finishNewReconciliation(moved);
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce(), { timeout: 2000 });
      expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({
        path: moved.path, expectedRevision: moved.revision, content: `${alphaDocument.content}\nlocal edit`,
      }));
    } finally { await unmount(component); }
  });

  it.each([
    [false, "before"], [false, "after"], [true, "before"], [true, "after"],
  ] as const)("synchronizes affected tabs after a failed rename (directory: %s, reload: %s failure)", async (isDir, delivery) => {
    const intervals = vi.spyOn(globalThis, "setInterval");
    const first = { ...alphaDocument, path: isDir ? "C:\\notes\\drafts\\Alpha.md" : alphaDocument.path };
    const second = { ...alphaDocument, id: "beta-document", path: "C:\\notes\\drafts\\Beta.md", title: "Beta.md", content: "# Beta snapshot" };
    const { component, target } = await mountReady(first);
    const poll = intervals.mock.calls.find(([, delay]) => delay === 2200)?.[0];
    const entry = { name: isDir ? "drafts" : "Alpha.md", path: isDir ? "C:\\notes\\drafts" : first.path, isDir, depth: 0 };
    const firstDisk = { ...first, content: "new Alpha disk text", revision: { hash: "new-alpha", size: 19, modifiedMs: 2 } };
    const secondDisk = { ...second, content: "new Beta disk text", revision: { hash: "new-beta", size: 18, modifiedMs: 2 } };
    let finishOldReload!: (value: unknown) => void;
    let failRename!: (error: Error) => void;
    try {
      if (isDir) {
        mocks.openDialog.mockResolvedValueOnce(second.path);
        mocks.api.openPaths.mockResolvedValueOnce([second]);
        await clickMenuCommand(target, "Open file");
        await vi.waitFor(() => expect(target.querySelector('[data-tab-id="beta-document"]')).not.toBeNull());
        target.querySelector<HTMLElement>('[data-tab-id="alpha-document"]')!.click();
        await tick();
      }
      mocks.api.openWorkspace.mockResolvedValueOnce({ root: "C:\\notes", name: "notes", entries: [entry] });
      mocks.openDialog.mockResolvedValueOnce("C:\\notes");
      await clickMenuCommand(target, "Open folder");
      await vi.waitFor(() => expect(target.querySelector(".file-row .file-main")).not.toBeNull());
      mocks.api.checkExternalChanges.mockResolvedValueOnce([{
        documentId: first.id, path: first.path, kind: "modified", revision: firstDisk.revision,
      }]);
      mocks.api.reloadDocument
        .mockReturnValueOnce(new Promise(resolve => finishOldReload = resolve))
        .mockImplementation(async (id: string) => id === first.id ? firstDisk : secondDisk);
      if (typeof poll === "function") poll();
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
      vi.spyOn(window, "prompt").mockReturnValue(isDir ? "archive" : "Taken.md");
      mocks.api.renameWorkspaceEntry.mockReturnValueOnce(new Promise((_resolve, reject) => failRename = reject));
      target.querySelector(".file-row .file-main")!.dispatchEvent(new KeyboardEvent("keydown", { key: "F2", bubbles: true }));
      await vi.waitFor(() => expect(mocks.api.renameWorkspaceEntry).toHaveBeenCalledOnce());
      if (delivery === "before") {
        finishOldReload(firstDisk);
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(editorView(target).state.doc.toString()).toBe(first.content);
      }
      failRename(new Error("A file with this name already exists."));
      await vi.waitFor(() => expect(target.textContent).toContain("A file with this name already exists."));
      if (delivery === "after") {
        expect(mocks.api.reloadDocument.mock.calls.filter(([id]) => id === first.id)).toHaveLength(1);
        finishOldReload(firstDisk);
      }
      await vi.waitFor(() => expect(editorView(target).state.doc.toString()).toBe(firstDisk.content));
      for (const snapshot of isDir ? [secondDisk, firstDisk] : [firstDisk]) {
        target.querySelector<HTMLElement>(`[data-tab-id="${snapshot.id}"]`)!.click();
        await vi.waitFor(() => expect(editorView(target).state.doc.toString()).toBe(snapshot.content));
        expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe(snapshot.path);
        expect(target.querySelector(".conflict-banner")).toBeNull();
      }
      expect(mocks.api.reloadDocument).toHaveBeenCalledTimes(isDir ? 3 : 2);
      expect(target.textContent).toContain("A file with this name already exists.");
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nnext edit" } });
      // The failed rename releases the automatic-save suspension only after
      // every affected tab has been synchronized.
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce(), { timeout: 2000 });
      expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({
        path: first.path, expectedRevision: firstDisk.revision, content: `${firstDisk.content}\nnext edit`,
      }));
    } finally { await unmount(component); }
  });

  it("keeps edits made while a successful rename synchronizes the new path", async () => {
    const { component, target } = await mountReady();
    const entry = { name: "Alpha.md", path: alphaDocument.path, isDir: false, depth: 0 };
    const moved = { ...alphaDocument, path: "C:\\notes\\Renamed.md", title: "Renamed.md", content: "external disk text", revision: { hash: "external", size: 18, modifiedMs: 2 } };
    let finishReconciliation!: (value: unknown) => void;
    try {
      mocks.api.openWorkspace.mockResolvedValueOnce({ root: "C:\\notes", name: "notes", entries: [entry] });
      mocks.openDialog.mockResolvedValueOnce("C:\\notes");
      await clickMenuCommand(target, "Open folder");
      await vi.waitFor(() => expect(target.querySelector(".file-row .file-main")).not.toBeNull());
      vi.spyOn(window, "prompt").mockReturnValue("Renamed.md");
      mocks.api.renameWorkspaceEntry.mockResolvedValueOnce({ root: "C:\\notes", name: "notes", entries: [{ ...entry, name: moved.title, path: moved.path }] });
      mocks.api.reloadDocument.mockReturnValueOnce(new Promise(resolve => finishReconciliation = resolve));
      target.querySelector(".file-row .file-main")!.dispatchEvent(new KeyboardEvent("keydown", { key: "F2", bubbles: true }));
      await vi.waitFor(() => expect(mocks.api.reloadDocument).toHaveBeenCalledOnce());
      const view = editorView(target);
      view.dispatch({ changes: { from: view.state.doc.length, insert: "\nlocal edit" } });
      finishReconciliation(moved);
      await vi.waitFor(() => expect(target.querySelector(".conflict-banner")).not.toBeNull());
      expect(view.state.doc.toString()).toBe(`${alphaDocument.content}\nlocal edit`);
      expect(target.querySelector(".document-tab.active")?.getAttribute("title")).toBe(moved.path);
      expect(mocks.api.saveDocument).not.toHaveBeenCalled();
    } finally { await unmount(component); }
  });
});

describe("workspace search response races", () => {
  it("opens an existing preview tab at the search hit's source line", async () => {
    const content = Array.from({ length: 60 }, (_, index) => `line ${index + 1}`).join("\n");
    const { component, target } = await mountReady({ ...alphaDocument, content });
    mocks.api.openWorkspace.mockResolvedValueOnce({ root: "C:\\notes", name: "notes", entries: [] });
    mocks.api.searchWorkspace.mockResolvedValueOnce([{ path: alphaDocument.path, relativePath: "Alpha.md", line: 50, column: 1, preview: "line 50" }]);
    try {
      mocks.openDialog.mockResolvedValueOnce("C:\\notes");
      await clickMenuCommand(target, "Open folder");
      await vi.waitFor(() => expect(mocks.api.openWorkspace).toHaveBeenCalledOnce());
      target.querySelector<HTMLButtonElement>('[title="Preview mode"]')!.click();
      await tick();
      expect(target.querySelector(".cm-content")).toBeNull();
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "f", ctrlKey: true, shiftKey: true }));
      await tick();
      const input = target.querySelector<HTMLInputElement>("#workspace-search-input")!;
      input.value = "line 50";
      input.dispatchEvent(new Event("input", { bubbles: true }));
      await vi.waitFor(() => expect(target.querySelector(".search-panel .results button")).not.toBeNull());
      target.querySelector<HTMLButtonElement>(".search-panel .results button")!.click();
      await vi.waitFor(() => expect(target.querySelector(".search-panel")).toBeNull());
      const view = editorView(target);
      expect(view.state.doc.lineAt(view.state.selection.main.head).number).toBe(50);
      expect(target.querySelector(".preview-scroller")).toBeNull();
    } finally { await unmount(component); }
  });

  it.each([false, true])("discards an old workspace response (failure: %s)", async (fail) => {
    const { component, target } = await mountReady();
    mocks.api.openWorkspace.mockImplementation(async (root: string) => ({ root, name: root.slice(-1), entries: [] }));
    let finishSearch!: (hits: SearchHit[]) => void;
    let rejectSearch!: (error: Error) => void;
    mocks.api.searchWorkspace.mockReturnValueOnce(new Promise<SearchHit[]>((resolve, reject) => {
      finishSearch = resolve;
      rejectSearch = reject;
    }));
    try {
      mocks.openDialog.mockResolvedValueOnce("C:\\A");
      await clickMenuCommand(target, "Open folder");
      await vi.waitFor(() => expect(mocks.api.openWorkspace).toHaveBeenCalledOnce());
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "f", ctrlKey: true, shiftKey: true }));
      await tick();
      const input = target.querySelector<HTMLInputElement>("#workspace-search-input")!;
      input.value = "needle";
      input.dispatchEvent(new Event("input", { bubbles: true }));
      await vi.waitFor(() => expect(mocks.api.searchWorkspace).toHaveBeenCalledOnce());
      expect(mocks.api.searchWorkspace.mock.calls[0][0].root).toBe("C:\\A");
      mocks.openDialog.mockResolvedValueOnce("C:\\B");
      await clickMenuCommand(target, "Open folder");
      await vi.waitFor(() => expect(target.querySelector(".search-panel header span")?.textContent).toBe("B"));
      expect(target.querySelector(".search-panel")?.textContent).not.toContain("Searching");
      if (fail) rejectSearch(new Error("old workspace search failed"));
      else finishSearch([{ path: "C:\\A\\old.md", relativePath: "old.md", line: 1, column: 1, preview: "needle" }]);
      await new Promise(resolve => setTimeout(resolve, 0));
      await tick();
      expect(target.querySelectorAll(".search-panel .results button")).toHaveLength(0);
      expect(target.textContent).not.toContain("old workspace search failed");
      expect(input.value).toBe("needle");
      mocks.api.searchWorkspace.mockResolvedValueOnce([{
        path: "C:\\B\\new.md", relativePath: "new.md", line: 1, column: 1, preview: "needle",
      }]);
      input.dispatchEvent(new Event("input", { bubbles: true }));
      await vi.waitFor(() => expect(target.querySelector(".search-panel .results")?.textContent).toContain("new.md"));
      expect(mocks.api.searchWorkspace.mock.calls[1][0].root).toBe("C:\\B");
    } finally { await unmount(component); }
  });
});
