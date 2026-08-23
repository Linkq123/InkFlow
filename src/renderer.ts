import { invoke } from "@tauri-apps/api/core";
import "katex/dist/katex.min.css";
import "./app.css";
import "./renderer.css";
import {
  waitForImagesOrTimeout,
  waitForPromiseOrTimeout,
} from "./lib/async";
import { prepareExportDocument } from "./lib/markdown/export-document";

interface RenderWorkerRequest {
  protocol: "inkflow.renderer/v3";
  token: string;
  operation: "fragment" | "html" | "pdf";
  title: string;
  markdown: string;
  documentPath: string | null;
  workspaceRoot: string | null;
  temporaryOutputPath: string | null;
  allowRemoteImages: boolean;
  editorFont: string;
  pageSize: string | null;
  landscape: boolean | null;
}

let token = "";
let failureSent = false;

async function run(): Promise<void> {
  token = await invoke<string>("renderer_token");
  await invoke("renderer_trace", { token, stage: "module-started" });
  const request = await invoke<RenderWorkerRequest>("renderer_request", { token });
  await invoke("renderer_trace", { token, stage: "request-received" });
  if (request.protocol !== "inkflow.renderer/v3" || request.token !== token) {
    throw new Error("The renderer request did not match its private token.");
  }
  const html = await prepareExportDocument(request.markdown, {
    allowRemoteImages: request.allowRemoteImages,
    editorFont: request.editorFont,
    loadResource: request.documentPath
      ? (resource) => invoke<string>("renderer_load_resource", { token, resource })
      : undefined,
  });
  await invoke("renderer_trace", { token, stage: "markdown-rendered" });
  const target = document.getElementById("render-target");
  if (!target) throw new Error("The renderer target is unavailable.");
  target.innerHTML = html;
  void document.body.offsetHeight;
  await Promise.all([
    waitForPromiseOrTimeout(document.fonts.ready, 2_000),
    request.operation === "pdf"
      ? waitForImagesOrTimeout(target, 10_000)
      : Promise.resolve(),
  ]);
  await invoke("renderer_finish", { token, renderedHtml: html });
}

void run().catch(async (error: unknown) => {
  if (failureSent) return;
  failureSent = true;
  const message = error instanceof Error ? error.message : String(error);
  try {
    await invoke("renderer_fail", {
      token,
      code: "renderer_frontend_error",
      message,
    });
  } catch {
    // The Rust worker will time out and report startup failure if IPC itself failed.
  }
});
