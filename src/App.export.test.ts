import { mount, tick, unmount } from "svelte";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { EditorView } from "@codemirror/view";
import { insertNewlineAndIndent } from "@codemirror/commands";
import type { ExternalChange } from "./lib/api/types";
import imageRewriteFixtures from "../tests/fixtures/image-rewrites.json";
import imageRewriteMerges from "../tests/fixtures/image-rewrite-merges.json";

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
    listRecovery: vi.fn(async () => []),
    markPerformanceReady: vi.fn(async () => true),
    checkExternalChanges: vi.fn(async (): Promise<ExternalChange[]> => []),
    reloadDocument: vi.fn(),
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
  mocks.api.saveDocument.mockReset().mockImplementation(async (request) => ({
    status: "saved", path: request.path, revision: alphaDocument.revision,
    content: null, recoveryWarnings: [],
  }));
  mocks.api.saveDocumentAs.mockReset();
  mocks.api.writeAsset.mockReset();
  mocks.api.checkExternalChanges.mockReset().mockResolvedValue([]);
  mocks.api.reloadDocument.mockReset();
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

describe("desktop export jobs", () => {
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

describe("Save As concurrent edits", () => {
  it.each(imageRewriteMerges.slice(0, 7))("resaves migrated paths in the edited syntax: $name", async ({ saved, rewritten, current, expected }) => {
    const { component, target } = await mountReady({ ...alphaDocument, content: saved });
    const path = "C:\\export\\Copy.md";
    mocks.saveDialog.mockResolvedValue(path);
    let finishSaveAs!: (value: unknown) => void;
    mocks.api.saveDocumentAs.mockReturnValueOnce(new Promise(resolve => finishSaveAs = resolve));
    try {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
      await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
      const view = editorView(target);
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: `${current}\n\nnew input` } });
      finishSaveAs(savedResult(rewritten, path));
      await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
      expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({
        path, content: `${expected}\n\nnew input`,
      }));
    } finally {
      await unmount(component);
    }
  });

  it.each(imageRewriteFixtures.slice(0, 3))("resaves migrated resources, not stale paths: $name", async ({ content, rewritten }) => {
    const { component, target } = await mountReady({ ...alphaDocument, content });
    const path = "C:\\export\\Copy.md";
    mocks.saveDialog.mockResolvedValue(path);
    let finishSaveAs!: (value: unknown) => void;
    mocks.api.saveDocumentAs.mockReturnValueOnce(new Promise(resolve => finishSaveAs = resolve));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "s", ctrlKey: true, shiftKey: true }));
    await vi.waitFor(() => expect(mocks.api.saveDocumentAs).toHaveBeenCalledOnce());
    const view = editorView(target);
    view.dispatch({ changes: { from: view.state.doc.length, insert: "\n\nnew input" } });
    finishSaveAs(savedResult(rewritten, path));
    await vi.waitFor(() => expect(mocks.api.saveDocument).toHaveBeenCalledOnce());
    expect(mocks.api.saveDocument.mock.calls[0][0]).toEqual(expect.objectContaining({
      path, content: `${rewritten}\n\nnew input`,
    }));
    await unmount(component);
  });
});
