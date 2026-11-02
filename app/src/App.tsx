import { createEffect, createRoot, on, onMount, Show } from "solid-js";
import { unwrap } from "solid-js/store";
import {
  api,
  confirmDiscard,
  confirmRestore,
  documentPath,
  exportJob,
  guardClose,
  isTauri,
  isSmathPath,
  isWeb,
  onMenu,
  onOpenFile,
  pickOpenPath,
  pickPdfPath,
  pickSavePath,
  setWindowTitle,
  isMac,
} from "./api";
import {
  addBackground,
  duplicateSelection,
  insertImage,
  insertImageBlob,
  insertImagePath,
  installContextMenus,
  isImagePath,
  onCopy,
  onPaste,
} from "./actions";
import { announce, cyclePanes, fromKeyboard, spoken, spokenNow, toolbarKeys } from "./a11y";
import { Canvas, clientToDoc, focusCanvas, reveal, zoomCenter, zoomToFit } from "./Canvas";
import { describeRegion } from "./describe";
import { CompletionPopup } from "./Completion";
import { ContextMenu } from "./ContextMenu";
import { FindBar } from "./FindBar";
import { importCsv, insertTable, isSheet, tableCommand } from "./TableEditor";
import { insertPlot } from "./Plot";
import { findStep, openFind } from "./find";
import { FormatBar } from "./FormatBar";
import { Inspector, setInspectorTab } from "./Inspector";
import { type Background, paperPoints } from "./pages";
import { NameTooltip } from "./Tooltip";
import { SettingsDialog } from "./SettingsDialog";
import { HelpDialog, setHelpOpen } from "./HelpDialog";
import { Palette, setPaletteOpen } from "./Palette";
import { RefPicker, setRefPickerOpen } from "./RefPicker";
import type { Doc } from "./types";
import { closeMenu, setMenu } from "./menu";
import { checks, failing, tableName } from "./checks";
import { PagePopover } from "./Popover";
import { checkForUpdates, UpdateBanner } from "./Updates";
import { setWelcomeOpen, Welcome } from "./Welcome";
import {
  adoptPath,
  flushSave,
  installAutosave,
  refresh as refreshNotebook,
  lastNotebook,
  nb,
  newNote,
  newSection,
  sidebarShown,
  toggleSidebar,
} from "./notebook";
import {
  focusNotebookSearch,
  newNotebookDialog,
  NoteTitle,
  openNotebookDialog,
  openNotebookAt,
  Sidebar,
  SidebarToggle,
} from "./Sidebar";
import { latestOnly } from "./latest";
import { keepRecovery } from "./recovery";
import { installTrace } from "./trace";
import { installRefs } from "./xref";
import { setTemplates, TemplatesDialog } from "./TemplatesDialog";
import { setShareOpen, ShareDialog } from "./ShareDialog";
import { decodeDoc, docInHash, linkFor } from "./share";
import { directPdf, pdfInfo, shrinkImages } from "./export";
import { addRecent, clearRecent, forgetRecent, recentList, syncRecent } from "./recent";
import { applyTheme, prefs, setSettingsOpen, withDefaults } from "./settings";
import { type Align, activeView, commands, run as runText } from "./richtext";
import {
  type AlignMode,
  alignRegions,
  commitEditing,
  convertEditing,
  deleteRegions,
  distributeRegions,
  editText,
  setAppCommand,
  flash,
  insertArea,
  insertAtCaret,
  insertContents,
  layout,
  loadDoc,
  localized,
  markSaved,
  redo,
  releaseErrors,
  selectAll,
  showReport,
  setState,
  settleImportedOnOpen,
  state,
  togglePageView,
  undo,
} from "./store";

function fileName(path: string | null): string {
  if (!path) return localized(state.doc.meta?.title) || "Untitled";
  return path
    .replace(/[\\/]+$/, "")
    .split(/[\\/]/)
    .pop()!
    .replace(/\.quirez?$/i, "");
}

// ---------------------------------------------------------------------------
// Live evaluation: every document change re-runs the worksheet (≈0.3 ms in Rust).
// ---------------------------------------------------------------------------

/** Waiting for a report of the document as it is now (see `afterEvaluation`). */
let waiting: (() => void)[] = [];

const liveReport = latestOnly(
  () => api.report(unwrap(state.doc)),
  (report) => {
    showReport(report);
    // only the newest request's answer arrives here: the results are now current
    const run = waiting;
    waiting = [];
    for (const fn of run) fn();
  },
  (e) => {
    waiting = [];
    flash(String(e));
  },
  30,
);

/** Run `fn` once the results on screen are those of the document as it is now. */
function afterEvaluation(fn: () => void) {
  if (liveReport.pending()) waiting.push(fn);
  else fn();
}

// whatever an edit held back shows as soon as it ends
createRoot(() => createEffect(on(() => state.editing, releaseErrors, { defer: true })));

const loadingAssets = new Set<string>();

function loadAssets() {
  // images inserted into an unsaved document are served from the staging area
  const bundle = state.path ?? state.origin ?? "";
  const backgrounds = (state.doc.page?.backgrounds as Background[] | undefined) ?? [];
  const sources = [...layout().list.map((p) => p.region.src), ...backgrounds.map((b) => b.src)];
  for (const src of sources) {
    if (!src || state.assets[src] || loadingAssets.has(src)) continue;
    loadingAssets.add(src);
    api
      .readAsset(bundle, src)
      .then((url) => setState("assets", src, url))
      .catch((e) => flash(`Image ${src}: ${e.message ?? e}`))
      .finally(() => loadingAssets.delete(src));
  }
}

// ---------------------------------------------------------------------------
// File commands
// ---------------------------------------------------------------------------

async function guardUnsaved(): Promise<boolean> {
  // a note in a notebook saves itself: finish that, nothing to ask
  if (nb.current) return flushSave().then(() => true);
  return !state.dirty || (await confirmDiscard());
}

export async function newDocument() {
  if (!(await guardUnsaved())) return;
  loadDoc(withDefaults(await api.newDocument()), null);
  adoptPath(null);
}

/**
 * An SMath worksheet opens as an untitled document, titled after the file. Save then asks for
 * a new .quire or .quirez file, so the .sm is never written over.
 */
function openImported(doc: Doc, file: string) {
  const name = file.split(/[\\/]/).pop() ?? "";
  loadDoc({ ...doc, meta: { ...doc.meta, title: name.replace(/\.sm$/i, "") } }, null);
  adoptPath(null);
  settleImportedOnOpen();
}

export async function openPath(path: string) {
  try {
    await flushSave();
    const doc = await api.open(path);
    if (isSmathPath(path)) return openImported(doc, path);
    loadDoc(doc, path);
    adoptPath(path);
    if (!nb.current) addRecent(path);
  } catch (e) {
    flash(`Could not open ${fileName(path)}: ${(e as Error).message}`);
    // a moved or deleted file shouldn't stay in Open Recent
    forgetRecent(path);
  }
}

async function openDocument() {
  if (!(await guardUnsaved())) return;
  if (isWeb) return openFile();
  const path = await pickOpenPath();
  if (path) await openPath(path);
}

async function save(as = false) {
  // ⌘S in a notebook just makes sure the note is on disk now
  if (nb.current && !as) return void flushSave();
  commitEditing();
  if (isWeb) return download();
  let path = state.path;
  if (!path || as) {
    path = await pickSavePath(fileName(state.path));
    if (!path) return;
  }
  // Save As from a note: the note itself keeps what was typed up to now
  if (nb.current) await flushSave();
  const rev = state.rev;
  try {
    await api.save(path, unwrap(state.doc), state.path ?? state.origin);
    setState({ path, origin: null });
    markSaved(rev);
    // the copy is the document now: autosave stops writing to the note, and the sidebar
    // shows the copy when it is a note of the notebook
    if (nb.book) {
      await refreshNotebook();
      adoptPath(path);
    }
    addRecent(path);
    flash(`Saved ${fileName(path)}`);
  } catch (e) {
    flash(`Could not save: ${(e as Error).message}`);
  }
}

// ---------------------------------------------------------------------------
// The web version: no files, only what the visitor picks, downloads or opens from a link
// ---------------------------------------------------------------------------

/** A document the visitor picked: a .quirez, a document.json from a .quire folder, or an SMath .sm. */
async function openFile() {
  const file = await new Promise<File | null>((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".quirez,.json,.sm";
    input.onchange = () => resolve(input.files?.[0] ?? null);
    input.click();
  });
  if (!file) return;
  try {
    const bytes = new Uint8Array(await file.arrayBuffer());
    const doc = await api.openBytes(base64(bytes));
    if (isSmathPath(file.name)) openImported(doc, file.name);
    else loadDoc(doc, null);
  } catch (e) {
    flash(`Could not open ${file.name}: ${(e as Error).message}`);
  }
}

/** A document a link points to (`?open=https://…`): a .quirez or document.json anywhere public. */
async function openUrl(url: string) {
  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`the server answered ${res.status}`);
    const doc = await api.openBytes(base64(new Uint8Array(await res.arrayBuffer())));
    // an SMath worksheet opens as an import: titled after the file, its regions settled
    const file = new URL(url, location.href).pathname.split("/").pop() ?? url;
    if (isSmathPath(file)) openImported(doc, file);
    else loadDoc(doc, null);
  } catch (e) {
    flash(`Could not open ${url}: ${(e as Error).message}`);
  }
}

/** A document carried in the link itself (`#doc=…`). */
async function openLinked(encoded: string) {
  try {
    loadDoc(await decodeDoc(encoded), null);
  } catch (e) {
    flash(`Could not open this link: ${(e as Error).message}`);
  }
}

/** Save in a browser: download the document as one .quirez file, images included. */
async function download() {
  const rev = state.rev;
  try {
    const bytes = Uint8Array.from(atob(await api.saveBytes(unwrap(state.doc))), (c) => c.charCodeAt(0));
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([bytes], { type: "application/zip" }));
    a.download = `${fileName(null).replace(/[\\/:*?"<>|]/g, "-")}.quirez`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
    markSaved(rev);
    flash(`Downloaded ${a.download}`);
  } catch (e) {
    flash(`Could not save: ${(e as Error).message}`);
  }
}

function base64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

/**
 * In the web version the address bar always holds the document as it is now: reloading
 * keeps your work, and copying the address shares it.
 */
function keepLinkCurrent() {
  let timer: ReturnType<typeof setTimeout> | undefined;
  createRoot(() =>
    createEffect(
      on(
        () => state.rev,
        () => {
          clearTimeout(timer);
          timer = setTimeout(async () => {
            const link = await linkFor(unwrap(state.doc), location.href).catch(() => null);
            if (link) history.replaceState(history.state, "", link);
          }, 800);
        },
        { defer: true },
      ),
    ),
  );
}

// ---------------------------------------------------------------------------
// Print / PDF: the page view *is* the print layout.
// ---------------------------------------------------------------------------

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Wait until evaluation, images, fonts and pagination have settled. */
async function settle() {
  await document.fonts.ready;
  for (let i = 0; i < 100 && (liveReport.pending() || loadingAssets.size > 0); i++) await sleep(50);
  // pagination reacts to measured heights; give it a few rounds to converge
  let last = "";
  for (let i = 0; i < 20; i++) {
    await sleep(80);
    const sig = layout()
      .list.map((p) => `${p.page}:${Math.round(p.top)}`)
      .join(",");
    if (sig === last) break;
    last = sig;
  }
}

async function withPrintLayout(run: () => Promise<unknown>) {
  commitEditing();
  const g = layout().geom;
  const pageRule = document.createElement("style");
  pageRule.textContent = `@page { size: ${g.paperW}px ${g.paperH}px; margin: 0; }`;
  document.head.appendChild(pageRule);
  setState({ printing: true, selection: [], caret: null });
  let restoreImages = () => {};
  try {
    await settle();
    restoreImages = await shrinkImages(prefs.exportDpi);
    await run();
  } finally {
    restoreImages();
    setState({ printing: false });
    pageRule.remove();
  }
}

const docTitle = () => fileName(state.path);

async function exportPdf(target?: string) {
  // Windows saves PDFs from the system print dialog
  if (isTauri && !directPdf && !target) return printDocument();
  const path = target ?? (await pickPdfPath(`${docTitle()}.pdf`));
  if (!path) return;
  await withPrintLayout(async () => {
    if (isTauri)
      await api.exportPdf(path, paperPoints(layout().geom), docTitle(), prefs.compressPhotos, pdfInfo(docTitle()));
    else window.print();
  });
  if (isTauri) flash(`Exported ${path.split("/").pop()}`);
}

async function printDocument() {
  await withPrintLayout(async () => {
    // Print stays a dialog everywhere; only macOS hands the job to the native print engine
    if (isTauri && isMac) await api.print(paperPoints(layout().geom), docTitle());
    // blocks until the dialog closes, so the print layout stays up while it previews
    else window.print();
  });
}

async function runExportJob(job: { open: string; pdf: string }) {
  // a headless run must end: if a step never finishes, say which one instead of hanging
  let step = "opening the document";
  const watchdog = setTimeout(() => void api.finishExport(`gave up after 100 s while ${step}`), 100_000);
  try {
    const doc = await api.open(job.open);
    loadDoc(doc, job.open);
    step = "waiting for results and pagination";
    await settle();
    step = "printing the PDF";
    await exportPdf(job.pdf);
    clearTimeout(watchdog);
    await api.finishExport(null);
  } catch (e) {
    clearTimeout(watchdog);
    await api.finishExport(String((e as Error).message ?? e));
  }
}

/** Insert ▾ in the toolbar: everything but math and text, which have their own buttons. */
function openInsertMenu(e: MouseEvent) {
  const box = (e.currentTarget as HTMLElement).getBoundingClientRect();
  const item = (label: string, id: string, shortcut?: string) => ({
    label,
    shortcut,
    run: () => (closeMenu(), runCommand(id)),
  });
  setMenu({
    x: box.left,
    y: box.bottom + 4,
    items: [
      item("Image…", "insert_image"),
      item("Table", "insert_table", "⌥⌘T"),
      item("Table from CSV…", "import_csv"),
      item("Plot", "insert_plot", "⌥⌘P"),
      item("Area", "insert_area"),
      null,
      item("Contents", "insert_toc"),
      item("List of Figures", "insert_figures"),
      item("List of Tables", "insert_tables"),
      item("Summary of Checks", "insert_checks"),
    ],
  });
}

/** The toolbar's zoom percentage: common sizes, and fitting the page to the window. */
function openZoomMenu(e: MouseEvent) {
  const box = (e.currentTarget as HTMLElement).getBoundingClientRect();
  const to = (z: number) => () => (closeMenu(), zoomCenter(z / state.view.zoom));
  const pages = layout().pageView;
  setMenu({
    x: box.right - 180,
    y: box.bottom + 4,
    items: [
      ...[50, 75, 100, 125, 150, 200].map((p) => ({
        label: `${p}%`,
        checked: Math.round(state.view.zoom * 100) === p,
        run: to(p / 100),
      })),
      null,
      { label: pages ? "Fit Page Width" : "Fit Printable Width", run: () => (closeMenu(), zoomToFit("width")) },
      ...(pages ? [{ label: "Fit Whole Page", run: () => (closeMenu(), zoomToFit("page")) }] : []),
      null,
      { label: "Zoom In", shortcut: "⌘=", run: () => (closeMenu(), zoomCenter(1.2)) },
      { label: "Zoom Out", shortcut: "⌘-", run: () => (closeMenu(), zoomCenter(1 / 1.2)) },
    ],
  });
}

/** One line of a status menu: what the region says, shortened. */
function regionText(id: string): string {
  const r = layout().byId.get(id)?.region;
  const text = r?.kind === "math" ? (r.expr ?? "") : r?.kind === "table" ? tableName(r) : (r?.kind ?? "");
  return text.length > 34 ? `${text.slice(0, 33)}…` : text;
}

/** The toolbar's error count: every error, in reading order; click one to go there. */
function openErrorsMenu(e: MouseEvent) {
  const box = (e.currentTarget as HTMLElement).getBoundingClientRect();
  const errors = layout().list.filter((p) => !p.hidden && state.views[p.region.id]?.error);
  setMenu({
    x: box.left,
    y: box.bottom + 4,
    items: errors.map((p) => {
      const msg = state.views[p.region.id]!.error!;
      return {
        label: `${regionText(p.region.id)} — ${msg.length > 60 ? `${msg.slice(0, 59)}…` : msg}`,
        run: () => (closeMenu(), reveal(p.region.id)),
      };
    }),
  });
}

/** The toolbar's checks: failing ones first, each one click away. */
function openChecksMenu(e: MouseEvent) {
  const box = (e.currentTarget as HTMLElement).getBoundingClientRect();
  const list = [...failing(), ...checks().filter((c) => c.pass)];
  setMenu({
    x: box.left,
    y: box.bottom + 4,
    items: list.map((c) => ({
      label: `${c.pass ? "✓" : "✗"}  ${c.cell ? `${regionText(c.id)} · ${c.row || `row ${c.cell[0] + 1}`}` : regionText(c.id)}`,
      run: () => (closeMenu(), reveal(c.id)),
    })),
  });
}

function nextError() {
  const errors = Object.values(state.views).filter((v) => v.error);
  if (!errors.length) return;
  const current = errors.findIndex((v) => v.id === state.selection[0]);
  reveal(errors[(current + 1) % errors.length].id);
}

const TEXT_FORMAT: Record<string, () => void> = {
  fmt_bold: () => runText(commands.bold),
  fmt_italic: () => runText(commands.italic),
  fmt_underline: () => runText(commands.underline),
  fmt_strike: () => runText(commands.strike),
  fmt_bullets: () => runText(commands.bulletList),
  fmt_numbers: () => runText(commands.orderedList),
};

function showInspector(tab: "region" | "document" | "page") {
  setInspectorTab(tab);
  setState({ inspector: true });
}

function runCommand(id: string) {
  if (tableCommand(id)) return;
  // a table being edited (not a cell being typed in) undoes like the canvas
  const inTable =
    isSheet(state.editing ? layout().byId.get(state.editing)?.region : undefined) &&
    !(document.activeElement instanceof HTMLInputElement);
  const editingField = !!state.editing && !inTable;
  if (TEXT_FORMAT[id]) return activeView() ? TEXT_FORMAT[id]() : undefined;
  if (id.startsWith("fmt_align_")) return activeView() && runText(commands.align(id.slice(10) as Align));
  if (id.startsWith("arrange_")) return alignRegions(state.selection, id.slice(8) as AlignMode);
  if (id.startsWith("recent:")) {
    const path = recentList()[Number(id.slice(7))];
    if (path) void guardUnsaved().then((ok) => (ok ? openPath(path) : undefined));
    return;
  }
  switch (id) {
    case "find":
    case "find_replace":
      commitEditing();
      return openFind(id === "find_replace");
    case "find_next":
      return findStep(1);
    case "find_prev":
      return findStep(-1);
    case "help":
      commitEditing();
      return setHelpOpen(true);
    case "command_palette":
      return setPaletteOpen(true);
    case "new_from_template":
      commitEditing();
      return setTemplates("new");
    case "save_template":
      commitEditing();
      return setTemplates("save");
    case "recent_clear":
      return clearRecent();
    case "distribute_x":
      return distributeRegions(state.selection, "x");
    case "distribute_y":
      return distributeRegions(state.selection, "y");
    case "duplicate":
      return duplicateSelection();
    case "insert_image":
      return insertImage().catch((e) => flash(`Could not insert the image: ${e.message ?? e}`));
    case "insert_table":
      return insertTable();
    case "import_csv":
      return void importCsv();
    case "insert_plot":
      return insertPlot();
    case "welcome":
      return setWelcomeOpen(true);
    case "check_updates":
      return void checkForUpdates(true);
    case "insert_ref":
      // a reference goes into text, at the caret
      if (activeView()) return setRefPickerOpen(true);
      return flash("Put the cursor in text first");
    case "insert_toc":
      return insertContents("headings");
    case "insert_figures":
      return insertContents("figures");
    case "insert_tables":
      return insertContents("tables");
    case "insert_checks":
      return insertContents("checks");
    case "toggle_inspector":
      return setState({ inspector: !state.inspector });
    case "region_properties":
      return showInspector("region");
    case "document_settings":
      return showInspector("document");
    case "page_setup":
      return showInspector("page");
    case "new":
      return nb.book ? newNote() : newDocument();
    case "new_document":
      return newDocument();
    case "new_section":
      return nb.book ? newSection("") : openNotebookDialog();
    case "new_notebook":
      return newNotebookDialog();
    case "open_notebook":
      return openNotebookDialog();
    case "toggle_sidebar":
      return toggleSidebar();
    case "search_notebook":
      return nb.book ? focusNotebookSearch() : openNotebookDialog();
    case "open":
      return openDocument();
    case "share_link":
      return setShareOpen(true);
    case "save":
      return save();
    case "save_as":
      return save(true);
    case "undo":
      if (activeView()) return runText(commands.undo);
      return editingField ? document.execCommand("undo") : undo();
    case "redo":
      if (activeView()) return runText(commands.redo);
      return editingField ? document.execCommand("redo") : redo();
    case "select_all":
      return editingField ? document.execCommand("selectAll") : selectAll();
    case "delete":
      return editingField ? document.execCommand("delete") : deleteRegions(state.selection);
    case "settings":
      commitEditing();
      return setSettingsOpen(true);
    case "insert_math":
      // inside text: live math at the caret; elsewhere a math region
      return activeView() ? runText(commands.insertMath) : insertAtCaret("math");
    case "insert_text": {
      // while typing math, the line becomes text instead
      const editing = state.editing ? layout().byId.get(state.editing)?.region : undefined;
      if (editing?.kind === "math") return convertEditing("text", editText(editing));
      return insertAtCaret("text");
    }
    case "insert_area":
      return insertArea();
    case "zoom_in":
      return zoomCenter(1.2);
    case "zoom_out":
      return zoomCenter(1 / 1.2);
    case "zoom_reset":
      return zoomCenter("reset");
    case "zoom_fit_width":
      return zoomToFit("width");
    case "zoom_fit_page":
      return zoomToFit("page");
    case "next_error":
      return nextError();
    case "page_view":
      return togglePageView();
    case "export_pdf":
      return exportPdf().catch((e) => flash(`Could not export: ${e.message ?? e}`));
    case "print":
      return printDocument().catch((e) => flash(`Could not print: ${e.message ?? e}`));
  }
}

/** In a plain browser there is no native menu; map the same shortcuts. */
function browserShortcuts(e: KeyboardEvent) {
  if (isTauri || !(e.metaKey || e.ctrlKey)) return;
  // ⌥ changes e.key on a Mac (⌥⌘F gives ƒ)
  const key = e.altKey && e.code.startsWith("Key") ? e.code.slice(3).toLowerCase() : e.key.toLowerCase();
  const map: Record<string, string> = {
    s: e.shiftKey ? "save_as" : "save",
    o: e.shiftKey ? "open_notebook" : "open",
    n: e.altKey ? "new_section" : e.shiftKey ? "new_from_template" : "new",
    "\\": "toggle_sidebar",
    z: e.shiftKey ? "redo" : "undo",
    p: e.altKey ? "insert_plot" : e.shiftKey ? "page_view" : "print",
    e: "export_pdf",
    d: "duplicate",
    i: e.altKey ? "toggle_inspector" : "",
    "=": "zoom_in",
    "-": "zoom_out",
    "0": "zoom_reset",
    "'": "next_error",
    f: e.altKey ? "find_replace" : e.shiftKey ? "search_notebook" : "find",
    m: e.shiftKey ? "insert_math" : "",
    r: e.shiftKey ? "insert_ref" : "",
    t: e.altKey ? "insert_table" : "",
    "?": "help",
    "/": e.shiftKey ? "help" : "",
    // Ctrl+G is Greek in math
    g: e.metaKey ? (e.shiftKey ? "find_prev" : "find_next") : "",
    ",": e.altKey ? "" : "settings",
  };
  if (key === "a" && state.editing) return;
  const cmd = key === "a" ? "select_all" : map[key];
  if (cmd && !(state.editing && (cmd === "undo" || cmd === "redo" || cmd === "duplicate"))) {
    e.preventDefault();
    runCommand(cmd);
  }
}

if (import.meta.env.DEV) (window as unknown as Record<string, unknown>).__quire = { state, layout };

/** What a screen reader hears without asking: messages, new errors, regions selected from the keyboard. */
function speakChanges() {
  createEffect(
    on(
      () => state.message,
      (m) => m && announce(m),
    ),
  );
  // the region just edited, once the edit ends and evaluation catches up: its error, or
  // for math its result (the line is hidden while it is edited, so it was never heard)
  let edits = 0;
  let resultDue: string | null = null;
  createEffect(
    on(
      () => state.editing,
      (now, before) => {
        if (!before || now) return;
        const id = before;
        const edit = ++edits;
        resultDue = layout().byId.get(id)?.region.kind === "math" ? id : null;
        // after the rest of the edit's ending (Esc selecting the region, a typed unit applied)
        queueMicrotask(() =>
          afterEvaluation(() => {
            // typing again, or a later edit ended: that one speaks instead
            if (edit !== edits || state.editing) return;
            const due = resultDue === id;
            resultDue = null;
            const error = state.views[id]?.error;
            if (error) announce(`Error: ${error}`, true);
            else if (due && layout().byId.has(id)) announce(describeRegion(id));
          }),
        );
      },
    ),
  );
  createEffect(
    on(
      () => state.selection.join(),
      () => {
        if (!fromKeyboard() || state.editing) return;
        const n = state.selection.length;
        if (!n) return;
        // Esc leaves the region just edited selected: it is described once its result is in
        if (n === 1 && state.selection[0] === resultDue) return;
        // moved on before it was: the region now selected is what matters (an error still speaks)
        resultDue = null;
        if (n === 1) announce(describeRegion(state.selection[0]));
        else if (n > 1) announce(`${n} regions selected`);
      },
      { defer: true },
    ),
  );
}

export function App() {
  createEffect(on(() => state.rev, liveReport.schedule));
  speakChanges();
  createEffect(on(() => [state.path, state.origin, layout()] as const, loadAssets));
  createEffect(() => {
    void setWindowTitle(`${fileName(state.path)}${state.dirty ? " — Edited" : ""}`);
  });

  onMount(async () => {
    installContextMenus(runCommand);
    setAppCommand(runCommand);
    installTrace();
    installRefs(reveal);
    // ⌘K from anywhere, the math being edited included (it keeps its keys to itself)
    window.addEventListener(
      "keydown",
      (e) => {
        if ((isMac ? e.metaKey : e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k") {
          e.preventDefault();
          e.stopPropagation();
          setPaletteOpen(true);
        }
      },
      true,
    );
    // F6 / ⇧F6: between the toolbar, the format bar, the page and the inspector
    window.addEventListener(
      "keydown",
      (e) => {
        if (e.key !== "F6" || e.metaKey || e.altKey) return;
        e.preventDefault();
        e.stopPropagation();
        cyclePanes(e.shiftKey ? -1 : 1, focusCanvas);
      },
      true,
    );
    // the window itself never scrolls (focus can try to)
    window.addEventListener("scroll", () => window.scrollTo(0, 0));
    document.getElementById("root")?.addEventListener("scroll", (e) => {
      const el = e.currentTarget as HTMLElement;
      el.scrollTop = 0;
      el.scrollLeft = 0;
    });
    document.addEventListener("copy", (e) => onCopy(e, false));
    document.addEventListener("cut", (e) => onCopy(e, true));
    document.addEventListener("paste", (e) => void onPaste(e));
    // images dropped on the canvas
    window.addEventListener("dragover", (e) => e.preventDefault());
    window.addEventListener("drop", (e) => {
      e.preventDefault();
      const files = [...(e.dataTransfer?.files ?? [])].filter((f) => f.type.startsWith("image/"));
      const at = clientToDoc(e.clientX, e.clientY);
      files.forEach((f, i) => insertImageBlob(f, [at[0] + i * 18, at[1] + i * 18]).catch((err) => flash(String(err))));
    });
    if (isTauri) {
      const { getCurrentWebview } = await import("@tauri-apps/api/webview");
      await getCurrentWebview().onDragDropEvent((ev) => {
        if (ev.payload.type !== "drop") return;
        const ratio = window.devicePixelRatio || 1;
        const at = clientToDoc(ev.payload.position.x / ratio, ev.payload.position.y / ratio);
        ev.payload.paths
          .filter(isImagePath)
          .forEach((path, i) =>
            insertImagePath(path, [at[0] + i * 18, at[1] + i * 18]).catch((err) => flash(String(err))),
          );
      });
    }
    void applyTheme();
    syncRecent();
    await onMenu(runCommand);
    await guardClose(() => state.dirty && !nb.current, api.clearRecovery, flushSave);
    installAutosave();
    window.addEventListener("keydown", browserShortcuts);
    if (exportJob) return runExportJob(exportJob);
    // another document's link pasted into the address bar
    window.addEventListener("hashchange", () => {
      const linked = docInHash(location.hash);
      if (linked) void guardUnsaved().then((ok) => (ok ? openLinked(linked) : undefined));
    });
    // a quiet look for a new version once the window has settled
    setTimeout(() => void checkForUpdates(), 4000);
    await onOpenFile(async (path) => {
      if (await guardUnsaved()) await openPath(path);
    });
    const requested = await api.takeOpenRequests().catch(() => [] as string[]);
    const recovered = await api.takeRecovery().catch(() => null);
    if (recovered?.doc && (await confirmRestore(fileName(recovered.path)))) {
      loadDoc(recovered.doc, recovered.path);
      setState({ dirty: true });
    } else if (docInHash(location.hash)) {
      await openLinked(docInHash(location.hash)!);
    } else if (isWeb) {
      const url = new URLSearchParams(location.search).get("open");
      if (url) await openUrl(url);
      else {
        loadDoc(
          await api
            .newDocument()
            .then(withDefaults)
            .catch(() => state.doc),
          null,
        );
        setWelcomeOpen(true);
      }
    } else {
      const params = new URLSearchParams(location.search);
      const open = requested.map(documentPath)[0] ?? params.get("open");
      // a notebook comes back as you left it, whatever the launch setting says about documents
      const book = params.get("notebook") ?? (open || params.has("open") ? null : lastNotebook());
      if (book && !open) {
        await openNotebookAt(book);
        if (nb.book) return keepRecovery();
      }
      const last = prefs.onLaunch === "last" ? recentList()[0] : undefined;
      if (open) await openPath(open);
      else if (last) await openPath(last).catch(() => undefined);
      if (!open && (!last || !state.path)) {
        loadDoc(
          await api
            .newDocument()
            .then(withDefaults)
            .catch(() => state.doc),
          null,
        );
        if (prefs.onLaunch === "welcome") setWelcomeOpen(true);
      }
    }
    keepRecovery();
    if (isWeb) keepLinkCurrent();
  });

  return (
    <div class="app" classList={{ tauri: isTauri, printing: state.printing, "with-sidebar": sidebarShown() }}>
      {/* with a notebook the sidebar stays mounted, so hiding and showing it can slide */}
      <Show when={nb.book || sidebarShown()}>
        <Sidebar collapsed={!sidebarShown()} />
      </Show>
      <div class="main">
        <header class="toolbar" data-tauri-drag-region>
          {/* the first column of the toolbar's grid: the sidebar button (when the sidebar is hidden) and the title */}
          <div class="toolbar-lead" data-tauri-drag-region>
            <Show when={!sidebarShown() && !isWeb}>
              <SidebarToggle />
            </Show>
            <h1 class="title" data-tauri-drag-region>
              <Show
                when={nb.current}
                fallback={
                  <>
                    {fileName(state.path)}
                    <Show when={state.dirty}>
                      <span class="edited"> — Edited</span>
                    </Show>
                  </>
                }
              >
                <NoteTitle />
              </Show>
            </h1>
          </div>
          <div class="tools" role="toolbar" aria-label="Insert" onKeyDown={(e) => toolbarKeys(e, focusCanvas)}>
            <button
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => runCommand("insert_math")}
              title="Math (⇧⌘M)"
            >
              <span class="glyph math-glyph" aria-hidden="true">
                x<sup>2</sup>
              </span>
              <span class="tool-label">Math</span>
            </button>
            <button onClick={() => insertAtCaret("text")} title="Text (⇧⌘T)">
              <span class="glyph" aria-hidden="true">
                ¶
              </span>{" "}
              <span class="tool-label">Text</span>
            </button>
            <button onClick={(e) => openInsertMenu(e)} title="Insert" aria-haspopup="menu">
              <span class="glyph" aria-hidden="true">
                ＋
              </span>{" "}
              <span class="tool-label">Insert</span>
            </button>
          </div>
          <div
            class="status"
            data-tauri-drag-region
            role="toolbar"
            aria-label="Document"
            onKeyDown={(e) => toolbarKeys(e, focusCanvas)}
          >
            <Show when={(state.report?.errorCount ?? 0) > 0}>
              <button class="errors" onClick={openErrorsMenu} title="Errors (⌘' for the next)" aria-haspopup="menu">
                {state.report!.errorCount} {state.report!.errorCount === 1 ? "error" : "errors"}
              </button>
            </Show>
            <Show when={checks().length}>
              <button
                class="checks-pill"
                classList={{ failing: failing().length > 0 }}
                onClick={openChecksMenu}
                title="List the checks"
                aria-haspopup="menu"
              >
                {failing().length
                  ? `✗ ${failing().length} of ${checks().length} failing`
                  : `✓ ${checks().length} ${checks().length === 1 ? "check" : "checks"}`}
              </button>
            </Show>
            <div class="segmented" role="group" aria-label="View">
              <button
                classList={{ on: !layout().pageView }}
                aria-pressed={!layout().pageView}
                onClick={() => layout().pageView && togglePageView()}
                title="Free canvas"
              >
                Canvas
              </button>
              <button
                classList={{ on: layout().pageView }}
                aria-pressed={layout().pageView}
                onClick={() => !layout().pageView && togglePageView()}
                title="Page view (⇧⌘P)"
              >
                Pages
              </button>
            </div>
            <button class="share-btn" onClick={() => runCommand("share_link")} title="Share a link">
              Share
            </button>
            <button
              class="icon-btn"
              classList={{ on: state.inspector }}
              onClick={() => runCommand("toggle_inspector")}
              title="Inspector (⌥⌘I)"
              aria-label="Inspector"
              aria-pressed={state.inspector}
            >
              <svg
                aria-hidden="true"
                width="16"
                height="16"
                viewBox="0 0 16 16"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
              >
                <rect x="1.5" y="2.5" width="13" height="11" rx="2" />
                <path d="M10 2.5v11" />
              </svg>
            </button>
            <button
              class="zoom"
              onClick={(e) => openZoomMenu(e)}
              title="Zoom"
              aria-label={`Zoom ${Math.round(state.view.zoom * 100)}%`}
              aria-haspopup="menu"
            >
              {Math.round(state.view.zoom * 100)}%
            </button>
          </div>
        </header>
        <FormatBar />
        <div class="workspace">
          <div class="canvas-pane">
            <Canvas />
            <FindBar />
          </div>
          <Show when={state.inspector}>
            <Inspector onAddBackground={() => addBackground().catch((e) => flash(String(e)))} />
          </Show>
        </div>
      </div>
      <ContextMenu />
      <CompletionPopup />
      <NameTooltip />
      <SettingsDialog />
      <HelpDialog />
      <Palette runCommand={runCommand} />
      <RefPicker />
      <ShareDialog />
      <PagePopover />
      <UpdateBanner beforeRestart={guardUnsaved} />
      <Welcome
        openPath={openPath}
        openDialog={() => runCommand("open")}
        openTemplate={async (doc, origin) => {
          if (!(await guardUnsaved())) return false;
          loadDoc(doc, null, origin);
          return true;
        }}
      />
      <TemplatesDialog
        onOpen={async (doc, origin) => {
          if (!(await guardUnsaved())) return false;
          loadDoc(doc, null, origin);
          return true;
        }}
        onBlank={newDocument}
      />
      <Show when={state.message}>
        <div class="toast" aria-hidden="true" onClick={() => setState({ message: null })}>
          {state.message}
        </div>
      </Show>
      {/* what a screen reader says on its own: messages, errors, the region the keyboard selected */}
      <div class="sr-only" role="status" aria-live="polite">
        {spoken()}
      </div>
      <div class="sr-only" role="alert" aria-live="assertive">
        {spokenNow()}
      </div>
    </div>
  );
}
