import { invoke } from "@tauri-apps/api/core";
import type {
  CheckpointRequest,
  DocumentSnapshot,
  ExportOutcome,
  ExportRequest,
  ExternalChange,
  RecoveryEntry,
  RecoverySnapshot,
  SaveDocumentRequest,
  SaveOutcome,
  SearchHit,
  SearchRequest,
  SettingsV1,
  WorkspaceSnapshot,
  WriteAssetRequest,
  WriteAssetResult,
} from "./types";

export function isDesktop(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!isDesktop()) {
    throw { code: "desktop_required", message: "This action is available in the desktop app." };
  }
  return invoke<T>(command, args);
}

export const api = {
  takeStartupPaths: () => call<string[]>("take_startup_paths"),
  openPaths: (paths: string[]) => call<DocumentSnapshot[]>("open_paths", { paths }),
  reloadDocument: (documentId: string) =>
    call<DocumentSnapshot>("reload_document", { documentId }),
  saveDocument: (request: SaveDocumentRequest) =>
    call<SaveOutcome>("save_document", { request }),
  saveDocumentAs: (request: SaveDocumentRequest) =>
    call<SaveOutcome>("save_document_as", { request }),
  checkExternalChanges: () => call<ExternalChange[]>("check_external_changes"),
  openWorkspace: (path: string) => call<WorkspaceSnapshot>("open_workspace", { path }),
  refreshWorkspace: () => call<WorkspaceSnapshot | null>("refresh_workspace"),
  searchWorkspace: (request: SearchRequest) =>
    call<SearchHit[]>("search_workspace", { request }),
  createWorkspaceEntry: (parent: string, name: string, isDir: boolean) =>
    call<WorkspaceSnapshot>("create_workspace_entry", { parent, name, isDir }),
  renameWorkspaceEntry: (path: string, newName: string) =>
    call<WorkspaceSnapshot>("rename_workspace_entry", { path, newName }),
  trashWorkspaceEntry: (path: string) =>
    call<WorkspaceSnapshot>("trash_workspace_entry", { path }),
  writeAsset: (request: WriteAssetRequest) =>
    call<WriteAssetResult>("write_asset", { request }),
  loadResource: (documentId: string, resource: string) =>
    call<string>("load_resource", { documentId, resource }),
  checkpointDocument: (request: CheckpointRequest) =>
    call<RecoveryEntry | null>("checkpoint_document", { request }),
  listRecovery: () => call<RecoveryEntry[]>("list_recovery"),
  restoreRevision: (id: string) => call<RecoverySnapshot>("restore_revision", { id }),
  deleteRecovery: (id: string) => call<void>("delete_recovery", { id }),
  getSettings: () => call<SettingsV1>("get_settings"),
  updateSettings: (settings: SettingsV1) =>
    call<SettingsV1>("update_settings", { settings }),
  exportHtml: (request: ExportRequest) => call<ExportOutcome>("export_html", { request }),
  exportPdf: (request: ExportRequest) => call<ExportOutcome>("export_pdf", { request }),
};

export function messageFromError(error: unknown): string {
  if (typeof error === "object" && error !== null && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}
