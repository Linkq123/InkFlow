import { mount, tick, unmount } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  saveDialog: vi.fn(),
  prepareExportDocument: vi.fn(async (_markdown: string, _options: unknown) =>
    "<p>Alpha snapshot</p>"),
  api: {
    takeStartupTargets: vi.fn(),
    openPaths: vi.fn(),
    saveDocument: vi.fn(),
    saveDocumentAs: vi.fn(),
    getSettings: vi.fn(),
    getSession: vi.fn(),
    updateSession: vi.fn(async (session) => session),
    updateSettings: vi.fn(async (settings) => settings),
    listRecovery: vi.fn(async () => []),
    markPerformanceReady: vi.fn(async () => true),
    checkExternalChanges: vi.fn(async () => []),
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
  confirm: vi.fn(async () => true),
  open: vi.fn(async () => null),
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

afterEach(() => {
  document.body.replaceChildren();
  document.body.classList.remove("printing");
  closeRequestedHandler = null;
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

function resetStartupMocks(): void {
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
  mocks.api.openPaths.mockResolvedValue([alphaDocument]);
  mocks.api.prepareExportSource.mockResolvedValue({ token: "source-1" });
  mocks.prepareExportDocument.mockResolvedValue("<p>Alpha snapshot</p>");
}

async function mountReady(): Promise<{ component: ReturnType<typeof mount>; target: HTMLElement }> {
  resetStartupMocks();
  const target = document.createElement("div");
  document.body.append(target);
  const component = mount(App, { target });
  await vi.waitFor(() => expect(target.textContent).toContain("Alpha.md"));
  return { component, target };
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
