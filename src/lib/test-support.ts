import type { DocumentTab } from "./api/types";
import { textFromString } from "./document-state";

export function newTabForTest(content: string, path: string | null): DocumentTab {
  return {
    id: crypto.randomUUID(),
    path,
    title: path?.split(/[\\/]/).pop() ?? "Untitled.md",
    content: textFromString(content),
    encoding: "utf-8",
    eol: "lf",
    hadBom: false,
    hadFinalNewline: false,
    readOnly: false,
    revision: null,
    editorVersion: 0,
    dirty: false,
    saveState: "saved",
    mode: "live",
    externalChange: null,
    allowRemoteImages: false,
  };
}
