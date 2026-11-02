// Bridge to the Rust backend: Tauri commands in the app, the engine compiled to WebAssembly
// in the web version, `quire serve` over /api in a browser while developing.
import { callEngine } from "./engine";
import type { PdfInfo } from "./export";
import type { Doc, Library, Report } from "./types";

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** The web version (`npm run build:web`): everything runs in the page, there are no files. */
export const isWeb = !isTauri && import.meta.env.VITE_QUIRE_WEB === "1";

async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isWeb) return callEngine<T>(cmd, args);
  if (isTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    try {
      return await invoke<T>(cmd, args);
    } catch (e) {
      throw new Error(String(e));
    }
  }
  const res = await fetch(`/api/${cmd}`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-quire-dev": "1" },
    body: JSON.stringify(args),
  });
  const body = await res.json().catch(() => {
    throw new Error(`the Quire backend is not reachable (HTTP ${res.status}); is \`quire serve\` running?`);
  });
  if (!res.ok) throw new Error(body?.error ?? res.statusText);
  return body as T;
}

export const api = {
  report: (doc: Doc) => call<Report>("report", { doc }),
  open: (path: string) => call<Doc>("open_document", { path }),
  save: (path: string, doc: Doc, from: string | null) => call<null>("save_document", { path, doc, from }),
  djotHtml: (src: string) => call<string>("djot_html", { src }),
  library: () => call<Library>("library"),
  importImage: (path: string) => call<string>("import_image", { path }),
  stageImage: (data: string, ext: string) => call<string>("stage_image", { data, ext }),
  readAsset: (bundle: string, src: string) => call<string>("read_asset", { bundle, src }),
  newDocument: () => call<Doc>("new_document"),
  listTemplates: () => call<TemplateInfo[]>("list_templates"),
  openTemplate: (id: string) => call<{ doc: Doc; origin: string | null }>("open_template", { id }),
  saveTemplate: (name: string, description: string, doc: Doc, from: string | null) =>
    call<string>("save_template", { name, description, doc, from }),
  deleteTemplate: (id: string) => call<null>("delete_template", { id }),
  print: (paper: Paper, title: string) => call<null>("print_document", { paper, title }),
  exportPdf: (path: string, paper: Paper, title: string, compress: boolean, info: PdfInfo) =>
    call<null>("export_pdf", { path, paper, title, compress, info }),
  finishExport: (error: string | null) => call<null>("finish_export", { error }),
  exportCsv: (path: string, contents: string) => call<null>("export_csv", { path, contents }),
  /** The web version's files, which travel as base64: a `.quirez` (or `document.json`) in, a `.quirez` out. */
  openBytes: (data: string) => call<Doc>("open_bytes", { data }),
  saveBytes: (doc: Doc) => call<string>("save_bytes", { doc }),
  /** Notebook operations (crates/quire-service/src/notebook.rs). */
  notebook: <T>(op: string, args: Record<string, unknown>) => call<T>("notebook", { op, args }),
  /** Documents the system asked to open before the UI was ready (Finder, Explorer, command line). */
  saveRecovery: (doc: Doc, path: string | null) =>
    isTauri ? call<null>("save_recovery", { doc, path }) : Promise.resolve(null),
  takeRecovery: () =>
    isTauri ? call<{ doc: Doc; path: string | null } | null>("take_recovery") : Promise.resolve(null),
  clearRecovery: () => (isTauri ? call<null>("clear_recovery") : Promise.resolve(null)),
  /** File ▸ Open Recent; the desktop app keeps only documents the user chose, and returns them. */
  setRecent: (paths: string[]) => (isTauri ? call<string[]>("set_recent", { paths }) : Promise.resolve(paths)),
  /** Hand the recent lists of versions that kept them only here to the desktop app (once). */
  adoptRecent: (documents: string[], notebooks: string[]) =>
    isTauri ? call<null>("adopt_recent", { documents, notebooks }) : Promise.resolve(null),
  /**
   * A native dialog. The desktop app runs it itself, so that what the user picks is all it may
   * read or write; it also adds the extension a saved file needs.
   */
  pick: (kind: PickKind, suggested?: string) => call<string | null>("pick", { kind, suggested: suggested ?? null }),
  takeOpenRequests: () => (isTauri ? call<string[]>("take_open_requests") : Promise.resolve([] as string[])),
};

export type PickKind = "open" | "save" | "pdf" | "csv" | "image" | "notebook" | "new_notebook";

export interface TemplateInfo {
  id: string;
  name: string;
  description: string;
  builtin: boolean;
}

export interface Paper {
  widthPt: number;
  heightPt: number;
}

/** Set by `quire-app --export-pdf <in> <out>` (headless export). */
export const exportJob = (window as unknown as { __QUIRE_EXPORT__?: { open: string; pdf: string } }).__QUIRE_EXPORT__;

/** Open Notebook…: a folder. */
export async function pickNotebookFolder(): Promise<string | null> {
  if (isTauri) return api.pick("notebook");
  return window.prompt("Path to a notebook folder");
}

/** New Notebook…: where to make its folder, and its name. */
export async function pickNewNotebookPath(): Promise<string | null> {
  if (isTauri) return api.pick("new_notebook", "Notebook");
  return window.prompt("Path of the new notebook folder");
}

export async function pickPdfPath(suggested: string): Promise<string | null> {
  if (isTauri) return api.pick("pdf", suggested);
  return suggested.toLowerCase().endsWith(".pdf") ? suggested : `${suggested}.pdf`;
}

export const isMac = /Mac/.test(navigator.userAgent);
export const isLinux = /Linux/.test(navigator.userAgent);

/**
 * Documents are `.quire` folders (a single file in the Finder, where they are packages)
 * or `.quirez` single files. Windows and Linux save single files: a folder that should
 * open like a file is not something those systems do.
 */
export const DOC_EXT = isMac ? "quire" : "quirez";

/** `…/x.quire/document.json` (picked in a file dialog) means the `x.quire` bundle. */
export function documentPath(p: string): string {
  return p.replace(/[\\/]document\.json$/i, "");
}

/** An SMath Studio worksheet (`.sm`, XML). It opens as an untitled document, never over the file. */
export const isSmathPath = (p: string) => /\.sm$/i.test(p);

export async function pickOpenPath(): Promise<string | null> {
  if (isTauri) return api.pick("open");
  return window.prompt("Path to a .quire or .quirez document");
}

export async function pickSavePath(suggested: string): Promise<string | null> {
  if (isTauri) return api.pick("save", `${suggested}.${DOC_EXT}`);
  const p = window.prompt("Save as (a path ending in .quire or .quirez)", `${suggested}.${DOC_EXT}`);
  if (!p) return null;
  return /\.quirez?$/i.test(p) ? p : `${p}.${DOC_EXT}`;
}

/** After a crash: put back the unsaved work? */
export async function confirmRestore(name: string): Promise<boolean> {
  const msg = `Quire closed before your changes to “${name}” were saved. Restore them?`;
  if (isTauri) {
    const { ask } = await import("@tauri-apps/plugin-dialog");
    return ask(msg, { title: "Restore unsaved changes", kind: "info", okLabel: "Restore", cancelLabel: "Discard" });
  }
  return window.confirm(msg);
}

export async function confirmDiscard(): Promise<boolean> {
  const msg = "You have unsaved changes. Discard them?";
  if (isTauri) {
    const { ask } = await import("@tauri-apps/plugin-dialog");
    return ask(msg, { title: "Quire", kind: "warning", okLabel: "Discard", cancelLabel: "Cancel" });
  }
  return window.confirm(msg);
}

/** Documents opened from the system while the app is running. */
export async function onOpenFile(handler: (path: string) => void) {
  if (!isTauri) return;
  const { listen } = await import("@tauri-apps/api/event");
  await listen<string>("open-file", (e) => handler(documentPath(e.payload)));
}

export async function onMenu(handler: (id: string) => void) {
  if (!isTauri) return;
  const { listen } = await import("@tauri-apps/api/event");
  await listen<string>("menu", (e) => handler(e.payload));
}

export async function setWindowTitle(title: string) {
  document.title = title;
  if (isTauri) {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setTitle(title);
  }
}

export async function guardClose(
  isDirty: () => boolean,
  onDiscard: () => Promise<unknown>,
  beforeClose?: () => Promise<unknown>,
) {
  if (!isTauri) {
    window.addEventListener("beforeunload", (e) => {
      if (isDirty()) e.preventDefault();
    });
    return;
  }
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().onCloseRequested(async (e) => {
    // a note in a notebook is written before the window goes
    await beforeClose?.().catch(() => {});
    if (!isDirty()) return;
    if (await confirmDiscard()) await onDiscard().catch(() => {});
    else e.preventDefault();
  });
}
