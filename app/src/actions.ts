// Document actions that span the UI: clipboard, images, duplicate, context menus.
import { copyPlotImage, editPlot } from "./Plot";
import { setCaptionToEdit } from "./Region";
import { unwrap } from "solid-js/store";
import { api, isTauri } from "./api";
import { latex } from "./latex";
import { cellText, columnsOf, exportCsv, insertTable, isSheet, parseTsv, rowsOf, tableMenuItems } from "./TableEditor";
import { mathml, prettyValue } from "./math";
import { looksLikeProse } from "./MathEditor";
import { captionLabel } from "./labels";
import { fillRefs } from "./xref";
import {
  assetRefs,
  type Clip,
  clipFromHtml,
  clipHtml,
  decodeClip,
  encodeClip,
  escapeHtml,
  htmlText,
  mathParagraph,
  piecesFromText,
  portableHtml,
  REGIONS_TYPE,
  tableFromHtml,
  tableHtml,
  tsv,
} from "./clipboard";
import { closeMenu, type MenuItems, setRegionMenuBuilder } from "./menu";
import type { Background } from "./pages";
import {
  availableWidth,
  commitEditing,
  contentBottom,
  convertRegion,
  deleteRegions,
  editText,
  findDraft,
  flash,
  GRID,
  heights,
  LINE,
  layout,
  localized,
  mutate,
  regroup,
  newId,
  type Placed,
  setState,
  snap,
  splitShow,
  startEditing,
  state,
  updateRegion,
} from "./store";
import type { Region } from "./types";

const IMAGE_EXT = /\.(png|jpe?g|gif|svg|webp)$/i;

// ---------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------

/** What was copied last in this window: the fallback when the system clipboard can't be read. */
let internalClipboard: Clip | null = null;

function cloneWithNewIds(r: Region): Region {
  const copy = structuredClone(unwrap(r)) as Region;
  const renew = (x: Region) => {
    x.id = newId();
    x.regions?.forEach(renew);
  };
  renew(copy);
  return copy;
}

/** The selected regions with absolute positions (children of areas lifted out). */
function selectedRegions(): Region[] {
  const ids = new Set(state.selection);
  return layout()
    .list.filter((p) => ids.has(p.region.id) && !(p.parent && ids.has(p.parent.id)))
    .map((p: Placed) => ({ ...structuredClone(unwrap(p.region)), at: [p.x, p.y] as [number, number] }));
}

/** Regions with the images they show, so they paste into another document too. */
function clipOf(regions: Region[]): Clip {
  const assets: Record<string, string> = {};
  for (const src of assetRefs(regions)) if (state.assets[src]) assets[src] = state.assets[src];
  return { regions, assets };
}

/** Rendered text (or a caption) as it reads on the page: live math, references, heading number. */
function renderedHtml(key: string, number?: string): string | null {
  const html = state.report?.html[key];
  if (html === undefined) return null;
  const el = document.createElement("div");
  el.innerHTML = html;
  for (const span of el.querySelectorAll<HTMLElement>("[data-math]")) {
    const v = state.views[span.dataset.math!];
    span.innerHTML = v?.input ? mathml(v.input, v.error ? null : v.result, v.units, v.constants, v.symbolic) : "";
  }
  fillRefs(el);
  const heading = el.querySelector("h1, h2, h3, h4, h5, h6");
  if (number && heading && state.doc.styles?.numbering?.headings !== "") heading.prepend(`${number}. `);
  return el.innerHTML;
}

/** A spreadsheet table's cells as they read on the page. */
function shownCells(r: Region): string[][] {
  const cells = state.views[r.id]?.cells;
  return rowsOf(r).map((row, i) => row.map((raw, j) => cellText(raw, cells?.[i]?.[j])));
}

function captionHtml(r: Region): string {
  if (!r.caption) return "";
  const number = layout().byId.get(r.id)?.number;
  const label = number ? `${captionLabel(r.kind === "table" ? "table" : "figure", number)} ` : "";
  const text = renderedHtml(`${r.id}:caption`) ?? escapeHtml(localized(r.caption.text, state.doc.meta?.lang));
  return `<p><i>${escapeHtml(label)}</i>${portableHtml(text)}</p>`;
}

/** One region for other apps: HTML, and plain text lines. */
function exportRegion(r: Region): { html: string; text: string } {
  const lang = state.doc.meta?.lang;
  switch (r.kind) {
    case "math": {
      const v = state.views[r.id];
      const line = editText(r);
      const html = v?.input
        ? mathParagraph(mathml(v.input, v.error ? null : v.result, v.units, v.constants, v.symbolic), line)
        : `<p>${escapeHtml(line)}</p>`;
      return { html, text: line };
    }
    case "text": {
      const html = renderedHtml(r.id, layout().byId.get(r.id)?.number);
      if (html === null) {
        const t = localized(r.text, lang);
        return { html: `<p>${escapeHtml(t)}</p>`, text: t };
      }
      return { html: portableHtml(html), text: htmlText(html) };
    }
    case "image": {
      const url = r.src ? state.assets[r.src] : undefined;
      const [w, h] = r.size ?? [200, 150];
      const img = url ? `<p><img src="${escapeHtml(url)}" width="${w}" height="${h}" alt=""></p>` : "";
      const caption = captionHtml(r);
      return { html: r.caption?.position === "above" ? caption + img : img + caption, text: "" };
    }
    case "area": {
      const title = localized(r.title, lang);
      const parts = [...(r.regions ?? [])].sort((a, b) => a.at[1] - b.at[1] || a.at[0] - b.at[0]).map(exportRegion);
      return {
        html: (title ? `<p><b>${escapeHtml(title)}</b></p>` : "") + parts.map((p) => p.html).join(""),
        text: [title, ...parts.map((p) => p.text)].filter(Boolean).join("\n"),
      };
    }
  }
  if (isSheet(r)) {
    const cells = shownCells(r);
    const table = tableHtml(cells, {
      headerRow: r.header?.row,
      headerColumn: r.header?.column,
      align: columnsOf(r).map((c) => c.align),
    });
    const caption = captionHtml(r);
    return { html: r.caption?.position === "below" ? table + caption : caption + table, text: tsv(cells) };
  }
  return { html: captionHtml(r), text: "" };
}

/** Text for the next copy event (Copy Result, Copy as LaTeX). */
let pendingText: string | null = null;

function copyText(text: string, what: string) {
  pendingText = text;
  const ok = document.execCommand("copy");
  pendingText = null;
  flash(ok ? `Copied ${what}` : "Could not copy");
}

/**
 * Copy the selected regions: as regions for Quire (any window, any document, after a
 * restart), as HTML for Word and the like, and as plain text.
 */
export function onCopy(e: ClipboardEvent, cut: boolean) {
  if (pendingText !== null) {
    e.clipboardData?.setData("text/plain", pendingText);
    e.preventDefault();
    return;
  }
  if (state.editing || isTextField(e.target) || !state.selection.length) return;
  const regions = selectedRegions();
  const clip = clipOf(regions);
  internalClipboard = clip;
  const parts = regions.map(exportRegion);
  e.clipboardData?.setData(REGIONS_TYPE, encodeClip(clip));
  e.clipboardData?.setData("text/html", clipHtml(parts.map((p) => p.html).join(""), clip));
  e.clipboardData?.setData(
    "text/plain",
    parts
      .map((p) => p.text)
      .filter(Boolean)
      .join("\n"),
  );
  e.preventDefault();
  if (cut) deleteRegions(state.selection);
}

function isTextField(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  return !!el?.closest?.("input:not(.key-sink), textarea:not(.key-sink), [contenteditable=true], .inspector");
}

export function pasteRegions(regions: Region[]) {
  if (!regions.length) return;
  commitEditing();
  const minX = Math.min(...regions.map((r) => r.at[0]));
  const minY = Math.min(...regions.map((r) => r.at[1]));
  const [tx, ty] = state.caret ?? [minX + GRID * 2, minY + GRID * 2];
  const pasted = regions.map((r) => {
    const c = cloneWithNewIds(r);
    c.at = [Math.max(0, snap(c.at[0] - minX + tx)), Math.max(0, snap(c.at[1] - minY + ty))];
    return c;
  });
  mutate((d) => {
    d.regions.push(...pasted);
  });
  regroup(pasted.map((r) => r.id));
  setState({ selection: pasted.map((r) => r.id), caret: null });
}

const IMAGE_TYPES: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/svg+xml": "svg",
  "image/webp": "webp",
};

/** Regions from another window or document: their images are added to this one first. */
async function pasteClip(clip: Clip) {
  const renamed = new Map<string, string>();
  for (const [src, url] of Object.entries(clip.assets)) {
    if (state.assets[src]) continue;
    const m = /^data:([^;,]+);base64,(.*)$/s.exec(url);
    const ext = m && IMAGE_TYPES[m[1]];
    if (!m || !ext) continue;
    try {
      const staged = await api.stageImage(m[2], ext);
      setState("assets", staged, url);
      if (staged !== src) renamed.set(src, staged);
    } catch (err) {
      flash(`Could not paste an image: ${err instanceof Error ? err.message : err}`);
    }
  }
  const relink = (r: Region) => {
    if (r.src && renamed.has(r.src)) r.src = renamed.get(r.src);
    r.regions?.forEach(relink);
  };
  const regions = structuredClone(clip.regions);
  regions.forEach(relink);
  pasteRegions(regions);
}

function escapeDjot(t: string) {
  return t.replace(/[\\*_[\]{}`~^$<>#|]/g, "\\$&");
}

/** Cells as a new table; a header row when marked so, or when labels sit over numbers. */
function pasteTable(rows: string[][], header: boolean) {
  commitEditing();
  const id = insertTable(rows);
  const numeric = (s: string) => /^[-+]?[\d.,]+/.test(s.trim());
  if (header || (rows.length > 1 && rows[0].every((c) => !numeric(c)) && rows.slice(1).some((r) => r.some(numeric))))
    updateRegion(id, { header: { row: true } });
  else updateRegion(id, { header: undefined });
}

/** Lines of text: one region each, math or text as if typed; LaTeX becomes linear math. */
function pasteLines(text: string) {
  const pieces = piecesFromText(text, (s) => looksLikeProse(s, ""));
  if (!pieces.length) return;
  commitEditing();
  const [x, y] = state.caret ?? [36, snap(contentBottom() + LINE)];
  const made: Region[] = pieces.map((p, i): Region => {
    const at: [number, number] = [x, y + i * 3 * GRID];
    if (p.kind === "text") return { id: newId(), kind: "text", at, text: escapeDjot(p.text) };
    const { expr, show, unit } = splitShow(p.text);
    return { id: newId(), kind: "math", at, expr, ...(show ? { show } : {}), ...(unit ? { display: { unit } } : {}) };
  });
  mutate((d) => {
    d.regions.push(...made);
  });
  regroup(made.map((r) => r.id));
  setState({ selection: made.map((r) => r.id), caret: null });
  if (made.length > 1) stackPasted(made.map((r) => r.id));
}

/**
 * Once pasted lines are drawn with their results, move each below the one before it if it
 * grew into it (a fraction, a matrix). Part of the paste's undo step.
 */
function stackPasted(ids: string[]) {
  let frames = 0;
  let settled = 0;
  const step = () => {
    const ready = ids.every((id) => heights[id] && (layout().byId.get(id)?.region.kind !== "math" || state.views[id]));
    // results arrive with the next evaluation; give their rendering a few frames
    if (++frames < 120 && (!ready || settled++ < 3)) return void requestAnimationFrame(step);
    mutate((d) => {
      let bottom = -Infinity;
      for (const id of ids) {
        const r = findDraft(d, id);
        if (!r) continue;
        if (r.at[1] < bottom + GRID) r.at = [r.at[0], Math.ceil((bottom + GRID) / GRID) * GRID];
        bottom = r.at[1] + (heights[id] ?? LINE);
      }
    }, false);
  };
  requestAnimationFrame(step);
}

/** What a paste offers, read from the paste event or the async clipboard API. */
interface Offer {
  regions?: string;
  html?: string;
  text?: string;
  image?: Blob;
}

/** Paste the best of what the clipboard offers; false when there was nothing to paste. */
async function pasteOffer(o: Offer): Promise<boolean> {
  const clip = decodeClip(o.regions) ?? clipFromHtml(o.html);
  if (clip) {
    await pasteClip(clip);
    return true;
  }
  const text = o.text?.trim() ?? "";
  const tsvRows = text.includes("\t") ? parseTsv(text) : null;
  const table = tableFromHtml(o.html);
  // cells copied in Quire carry their formulas in the text; from elsewhere the HTML table
  // is the better copy (cells with line breaks, merged cells)
  if (table && !(tsvRows && tsvRows.length === table.rows.length)) {
    pasteTable(table.rows, table.header);
    return true;
  }
  if (tsvRows) {
    pasteTable(tsvRows, !!table?.header);
    return true;
  }
  if (o.image) {
    await insertImageBlob(o.image);
    return true;
  }
  if (!text) return false;
  pasteLines(text);
  return true;
}

export async function onPaste(e: ClipboardEvent) {
  if (state.editing || isTextField(e.target)) return;
  const data = e.clipboardData;
  if (!data) return;
  // read everything now: the data is gone once the event is over
  const offer: Offer = {
    regions: data.getData(REGIONS_TYPE) || undefined,
    html: data.getData("text/html") || undefined,
    text: data.getData("text/plain") || undefined,
    image: [...data.files].find((f) => f.type.startsWith("image/")),
  };
  e.preventDefault();
  await pasteOffer(offer);
}

/** Paste from a menu, where there is no paste event: read the system clipboard. */
export async function pasteFromMenu() {
  closeMenu();
  try {
    const items = await navigator.clipboard.read();
    const offer: Offer = {};
    for (const item of items) {
      for (const type of item.types) {
        if (type === "text/html") offer.html ??= await (await item.getType(type)).text();
        else if (type === "text/plain") offer.text ??= await (await item.getType(type)).text();
        else if (type.startsWith("image/")) offer.image ??= await item.getType(type);
      }
    }
    if (await pasteOffer(offer)) return;
  } catch {
    // not allowed to read it: what was copied in this window is still at hand
  }
  if (internalClipboard) await pasteClip(internalClipboard);
  else flash("Nothing to paste");
}

export function copyToInternal() {
  internalClipboard = clipOf(selectedRegions());
}

export function duplicateSelection() {
  if (!state.selection.length) return;
  const regions = selectedRegions();
  setState({ caret: null });
  pasteRegions(regions);
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

function naturalSize(url: string): Promise<[number, number]> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve([img.naturalWidth || 300, img.naturalHeight || 200]);
    img.onerror = () => resolve([300, 200]);
    img.src = url;
  });
}

async function placeImage(src: string, at?: [number, number]) {
  const url = await api.readAsset(state.path ?? "", src);
  setState("assets", src, url);
  const [nw, nh] = await naturalSize(url);
  const [x, y] = at ?? state.caret ?? [GRID * 4, GRID * 4];
  const maxW = Math.min(availableWidth(x), 480);
  const w = Math.round(Math.min(nw, maxW));
  const id = newId();
  mutate((d) => {
    d.regions.push({ id, kind: "image", at: [snap(x), snap(y)], size: [w, Math.round((w * nh) / nw)], src });
  });
  regroup([id]);
  setState({ selection: [id], caret: null });
}

export async function insertImagePath(path: string, at?: [number, number]) {
  await placeImage(await api.importImage(path), at);
}

function readAsBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).split(",", 2)[1] ?? "");
    r.onerror = () => reject(r.error);
    r.readAsDataURL(blob);
  });
}

export async function insertImageBlob(file: Blob, at?: [number, number]) {
  const ext = (file.type.split("/")[1] ?? "png").replace("jpeg", "jpg").replace("svg+xml", "svg");
  await placeImage(await api.stageImage(await readAsBase64(file), ext), at);
}

/** Ask for an image file (Tauri dialog, or a file input in a browser). */
export async function pickImage(): Promise<{ path?: string; file?: File } | null> {
  if (isTauri) {
    const p = await api.pick("image");
    return p ? { path: p } : null;
  }
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/*";
    input.onchange = () => resolve(input.files?.[0] ? { file: input.files[0] } : null);
    input.click();
  });
}

export async function insertImage(at?: [number, number]) {
  commitEditing();
  const pick = await pickImage();
  if (pick?.path) await insertImagePath(pick.path, at);
  else if (pick?.file) await insertImageBlob(pick.file, at);
}

export async function addBackground() {
  const pick = await pickImage();
  if (!pick) return;
  const src = pick.path
    ? await api.importImage(pick.path)
    : await api.stageImage(
        await readAsBase64(pick.file!),
        (pick.file!.type.split("/")[1] ?? "png").replace("jpeg", "jpg"),
      );
  setState("assets", src, await api.readAsset(state.path ?? "", src));
  mutate((d) => {
    const page = (d.page ??= {});
    const list = ((page.backgrounds as Background[] | undefined) ?? []).concat([{ pages: "1", src, fit: "margins" }]);
    page.backgrounds = list;
  });
}

export const isImagePath = (p: string) => IMAGE_EXT.test(p);

// ---------------------------------------------------------------------------
// Context menus
// ---------------------------------------------------------------------------

/** More / Fewer Decimals for one result, as in a spreadsheet. */
function decimalsItems(id: string, r: Region): MenuItems {
  const docDecimals = (state.doc.settings as { display?: { decimals?: number } } | undefined)?.display?.decimals ?? 4;
  const current = r.display?.decimals ?? docDecimals;
  const set = (n: number) => () => {
    closeMenu();
    updateRegion(id, (d) => {
      d.display = { ...d.display, decimals: n === docDecimals ? undefined : n };
      if (d.display.decimals === undefined) delete d.display.decimals;
      if (!Object.keys(d.display).length) delete d.display;
    });
  };
  return [
    { label: `More Decimals (${current + 1})`, disabled: current >= 15, run: set(current + 1) },
    { label: `Fewer Decimals (${Math.max(0, current - 1)})`, disabled: current <= 0, run: set(current - 1) },
  ];
}

export function installContextMenus(run: (command: string) => void) {
  setRegionMenuBuilder((id): MenuItems => {
    const go = (cmd: string) => () => {
      closeMenu();
      run(cmd);
    };
    if (!id) {
      return [
        { label: "Paste", shortcut: "⌘V", run: () => void pasteFromMenu() },
        null,
        { label: "Insert Math", run: go("insert_math") },
        { label: "Insert Text", run: go("insert_text") },
        { label: "Insert Image…", run: go("insert_image") },
        { label: "Insert Table", run: go("insert_table") },
        { label: "Insert Table from CSV…", run: go("import_csv") },
        { label: "Insert Plot", run: go("insert_plot") },
        { label: "Insert Area", run: go("insert_area") },
        null,
        { label: "Table of Contents", run: go("insert_toc") },
        { label: "List of Figures", run: go("insert_figures") },
        { label: "List of Tables", run: go("insert_tables") },
        { label: "Summary of Checks", run: go("insert_checks") },
        null,
        { label: "Page View", checked: layout().pageView, run: go("page_view") },
        { label: "Document Settings…", run: go("document_settings") },
      ];
    }
    const r = layout().byId.get(id)?.region;
    const many = state.selection.length > 1;
    const items: MenuItems = [];
    if (!many && r?.kind === "text" && state.editing === id)
      items.push(
        { label: "Insert Math", shortcut: "⇧⌘M", run: go("insert_math") },
        { label: "Insert Cross-Reference…", shortcut: "⇧⌘R", run: go("insert_ref") },
        null,
      );
    if (!many && r && isSheet(r) && state.editing === id)
      items.push(...tableMenuItems((fn) => () => (closeMenu(), fn())));
    else if (!many && r?.kind === "plot2d")
      items.push({ label: "Edit Plot", shortcut: "↩", run: () => (closeMenu(), editPlot(id)) });
    else if (!many && r && state.editing !== id && (r.kind === "math" || r.kind === "text" || isSheet(r)))
      items.push({
        label: isSheet(r) ? "Edit Table" : "Edit",
        shortcut: "↩",
        run: () => (closeMenu(), startEditing(id)),
      });
    if (!many && r && isSheet(r))
      items.push({ label: "Export as CSV…", run: () => (closeMenu(), void exportCsv(id)) }, null);
    items.push(
      {
        label: "Cut",
        shortcut: "⌘X",
        run: () => (closeMenu(), copyToInternal(), document.execCommand("cut") || deleteRegions(state.selection)),
      },
      { label: "Copy", shortcut: "⌘C", run: () => (closeMenu(), copyToInternal(), document.execCommand("copy")) },
      { label: "Paste", shortcut: "⌘V", run: () => void pasteFromMenu() },
      { label: "Duplicate", shortcut: "⌘D", run: go("duplicate") },
      { label: "Delete", shortcut: "⌫", run: () => (closeMenu(), deleteRegions(state.selection)) },
      null,
    );
    if (many) {
      items.push(
        { label: "Align Left Edges", run: go("arrange_left") },
        { label: "Align Centres", run: go("arrange_hcenter") },
        { label: "Align Tops", run: go("arrange_top") },
        { label: "Distribute Horizontally", disabled: state.selection.length < 3, run: go("distribute_x") },
        { label: "Distribute Vertically", disabled: state.selection.length < 3, run: go("distribute_y") },
        null,
      );
    } else if (r?.kind === "math") {
      const view = state.views[id];
      items.push(
        {
          label: "Copy Result",
          disabled: !view?.resultText || !!view.error,
          run: () => (closeMenu(), copyText(prettyValue(view!.resultText!), "the result")),
        },
        {
          label: "Copy as LaTeX",
          disabled: !view?.input,
          run: () => (
            closeMenu(),
            copyText(latex(view!.input!, view!.error ? null : view!.result, view!.units), "as LaTeX")
          ),
        },
        null,
        {
          label: "Show Result",
          checked: !!r.show,
          run: () => (closeMenu(), updateRegion(id, { show: r.show ? undefined : "value" })),
        },
        ...(r.show === "value" ? decimalsItems(id, r) : []),
        {
          label: "Exclude from Calculation",
          checked: !!r.disabled,
          run: () => (closeMenu(), updateRegion(id, { disabled: r.disabled ? undefined : true })),
        },
        { label: "Convert to Text", run: () => (closeMenu(), commitEditing(), convertRegion(id, "text")) },
        null,
      );
    } else if (!many && r?.kind === "text" && state.editing !== id) {
      items.push({ label: "Convert to Math", run: () => (closeMenu(), convertRegion(id, "math")) }, null);
    } else if (r && ["image", "table", "plot2d"].includes(r.kind)) {
      if (r.kind === "plot2d") items.push({ label: "Copy as Image", run: () => (closeMenu(), void copyPlotImage(id)) });
      items.push(
        {
          label: r.caption ? "Remove Caption" : "Add Caption",
          run: () => {
            closeMenu();
            // r is live: read whether it had a caption before changing it
            const adding = !r.caption;
            updateRegion(id, {
              caption: adding ? { text: "Caption", position: r.kind === "table" ? "above" : "below" } : undefined,
            });
            // a new caption opens for typing
            if (adding) setCaptionToEdit(id);
          },
        },
        null,
      );
    }
    items.push({ label: "Properties…", run: go("region_properties") });
    return items;
  });
}
