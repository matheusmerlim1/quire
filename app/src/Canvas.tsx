import { createEffect, createMemo, createSignal, For, Index, on, onCleanup, onMount, Show } from "solid-js";
import { openRegionMenu, openRegionMenuAt } from "./menu";
import { SHEET_GAP } from "./pages";
import { forwardTyping } from "./MathEditor";
import { RegionBox, resizable, resizeBy } from "./Region";
import { announce } from "./a11y";
import { isMac } from "./api";
import { nearestRegion, stepRegion } from "./nav";
import { editPlot } from "./Plot";
import { Sheet } from "./Sheet";
import { welcomeOpen } from "./Welcome";
import { prefs } from "./settings";
import {
  checkpoint,
  commitEditing,
  contentBottom,
  deleteRegions,
  GRID,
  insertAtCaret,
  LINE,
  pagedLayout,
  shiftBelow,
  returnToLeftBox,
  type Placed,
  layout,
  moveTo,
  nudge,
  originsOf,
  regroup,
  heights,
  setRevealHandler,
  widths,
  setState,
  snap,
  startEditing,
  state,
  toggleArea,
} from "./store";

const MIN_ZOOM = 0.25;
const MAX_ZOOM = 4;

let viewportEl: HTMLDivElement | undefined;

export function zoomAt(clientX: number, clientY: number, factor: number) {
  if (!viewportEl) return;
  const rect = viewportEl.getBoundingClientRect();
  const { x, y, zoom } = state.view;
  const z = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, zoom * factor));
  const lx = clientX - rect.left;
  const ly = clientY - rect.top;
  setState("view", { zoom: z, x: lx - ((lx - x) * z) / zoom, y: ly - ((ly - y) * z) / zoom });
}

export function zoomCenter(factor: number | "reset") {
  if (!viewportEl) return;
  const rect = viewportEl.getBoundingClientRect();
  const f = factor === "reset" ? 1 / state.view.zoom : factor;
  zoomAt(rect.left + rect.width / 2, rect.top + rect.height / 3, f);
}

/** Room around the pages in page view. */
const PAGE_PAD = 32;

const [viewportSize, setViewportSize] = createSignal({ w: 0, h: 0 });

/** Height of the + New page button under the last page. */
const NEW_PAGE_H = 44;

/** The page view's pages, stacked (and the + New page button): width and height in scene px. */
function pagesExtent() {
  const l = layout();
  const button = state.printing ? 0 : SHEET_GAP + NEW_PAGE_H;
  return { w: l.geom.paperW, h: l.pages * (l.geom.paperH + SHEET_GAP) - SHEET_GAP + button };
}

/**
 * Page view: the pages centred while they fit across the window, and never scrolled off;
 * the free canvas goes on in every direction.
 */
function clampView(v: { x: number; y: number; zoom: number }) {
  const { w, h } = viewportSize();
  if (!layout().pageView || !w) return v;
  const e = pagesExtent();
  const sw = e.w * v.zoom;
  const sh = e.h * v.zoom;
  const within = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));
  return {
    ...v,
    x: sw + 2 * PAGE_PAD <= w ? (w - sw) / 2 : within(v.x, w - sw - PAGE_PAD, PAGE_PAD),
    y: sh + 2 * PAGE_PAD <= h ? PAGE_PAD : within(v.y, h - sh - PAGE_PAD, PAGE_PAD),
  };
}

/** View ▸ Zoom to Width / Zoom to Page: the page (or the canvas's printable column) fills the window. */
export function zoomToFit(kind: "width" | "page") {
  const { w, h } = viewportSize();
  if (!w) return;
  const l = layout();
  const { zoom, y } = state.view;
  const width = l.pageView ? l.geom.paperW : l.geom.contentW + 96;
  let z = (w - 2 * PAGE_PAD) / width;
  if (kind === "page" && l.pageView) z = Math.min(z, (h - 2 * PAGE_PAD) / l.geom.paperH);
  z = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, z));
  // keep the same part of the document at the top of the window
  let top = -y / zoom;
  if (kind === "page" && l.pageView) {
    const stride = l.geom.paperH + SHEET_GAP;
    top = Math.round(Math.max(0, top + h / 3 / zoom) / stride - 0.5) * stride;
  }
  setState("view", {
    zoom: z,
    x: l.pageView ? 0 : 48 * z,
    y: (kind === "page" && l.pageView ? PAGE_PAD : 0) - top * z,
  });
}

/** A window point in document coordinates (for drops). */
export function clientToDoc(cx: number, cy: number): [number, number] {
  if (!viewportEl) return [0, 0];
  const rect = viewportEl.getBoundingClientRect();
  const sx = (cx - rect.left - state.view.x) / state.view.zoom;
  const sy = (cy - rect.top - state.view.y) / state.view.zoom;
  const [x, y] = layout().fromScene(sx, sy);
  return [Math.max(0, snap(x)), Math.max(0, snap(y))];
}

/** Scroll so a region is in view and select it. */
export function reveal(id: string) {
  // open any collapsed areas around it first (undoable, like a manual expand)
  let parent = layout().byId.get(id)?.parent ?? null;
  while (parent) {
    if (parent.collapsed) toggleArea(parent.id);
    parent = layout().byId.get(parent.id)?.parent ?? null;
  }
  const p = layout().byId.get(id);
  if (!p || !viewportEl) return;
  const rect = viewportEl.getBoundingClientRect();
  const z = state.view.zoom;
  const [sx, sy] = layout().toScene(p.x, p.y);
  setState("view", { x: rect.width / 3 - sx * z, y: rect.height / 3 - sy * z });
  setState({ selection: [id], caret: null });
}

setRevealHandler(reveal);

let sinkEl: HTMLTextAreaElement | undefined;

/**
 * Give the page the keyboard (F6, Esc from a toolbar): typing then starts a region. With
 * nothing selected and no insertion point, the insertion point goes to the top left of
 * what is in view.
 */
export function focusCanvas() {
  if (!sinkEl || !viewportEl) return;
  // back into the region being edited, where it was left
  const editor =
    state.editing &&
    viewportEl.querySelector<HTMLElement>(".region.editing :is(input, [contenteditable=true], .sheet-wrap)");
  if (editor) return editor.focus({ preventScroll: true });
  if (!state.caret && !state.selection.length && !state.editing) {
    const rect = viewportEl.getBoundingClientRect();
    setState({ caret: clientToDoc(rect.left + 48, rect.top + 48) });
  }
  sinkEl.focus({ preventScroll: true });
}

/** Select a region, scrolling only if it is out of view (keyboard moves). */
export function selectInView(id: string) {
  const p = layout().byId.get(id);
  if (!p || !viewportEl) return;
  const rect = viewportEl.getBoundingClientRect();
  const z = state.view.zoom;
  const [sx, sy] = layout().toScene(p.x, p.y);
  const left = sx * z + state.view.x;
  const top = sy * z + state.view.y;
  const w = (widths[id] ?? 0) * z;
  const h = (heights[id] ?? 0) * z;
  if (p.hidden || left < 0 || top < 0 || left + w > rect.width || top + h > rect.height) return reveal(id);
  setState({ selection: [id], caret: null });
}

/**
 * The scene is zoomed with CSS `zoom`, which lays it out at size, rather than a scale
 * transform: WebKit draws a scaled scene as a bitmap while a text field inside it has the
 * caret, so editing went blurry, and switching between the two on every edit flickered.
 * Engines that report zoomed boxes differently from transforms keep the transform (checked
 * once). The scene is also kept on a layer of its own: WebKit spaces text slightly
 * differently inside one, and a text field with the caret otherwise moved it on and off
 * a layer, so text that wasn't even being edited shifted on every edit.
 */
const zoomKeepsCoordinates = (() => {
  const outer = document.createElement("div");
  outer.style.cssText = "position:absolute;visibility:hidden;zoom:2";
  const inner = document.createElement("div");
  inner.style.width = "10px";
  outer.append(inner);
  document.body.append(outer);
  const w = inner.getBoundingClientRect().width;
  outer.remove();
  return Math.abs(w - 20) < 0.5;
})();

function sceneStyle(): Record<string, string> {
  const { x, y, zoom } = state.view;
  if (zoomKeepsCoordinates && !state.printing)
    return { transform: `translate(${x / zoom}px, ${y / zoom}px)`, zoom: String(zoom), "will-change": "transform" };
  return { transform: `translate(${x}px, ${y}px) scale(${zoom})` };
}

/** Smart guides shown while dragging: a vertical and/or horizontal line, in scene px. */
const [guides, setGuides] = createSignal<{ v?: [number, number, number]; h?: [number, number, number] }>({});

const SNAP_PX = 6;

/**
 * Snap the dragged group to edges and centres of the other regions.
 * Returns the adjusted delta per axis, or null when nothing is close enough.
 */
function smartSnap(ids: Set<string>, origins: Map<string, [number, number]>, dx: number, dy: number) {
  const l = layout();
  const box = (id: string, x: number, y: number) => ({ x, y, w: widths[id] ?? 40, h: heights[id] ?? 20 });
  const moving = [...origins.keys()].map((id) => {
    const p = l.byId.get(id)!;
    return box(id, p.x + dx, p.y + dy);
  });
  if (!moving.length) return { dx: null, dy: null };
  const g = {
    x0: Math.min(...moving.map((b) => b.x)),
    x1: Math.max(...moving.map((b) => b.x + b.w)),
    y0: Math.min(...moving.map((b) => b.y)),
    y1: Math.max(...moving.map((b) => b.y + b.h)),
  };
  const others = l.list
    .filter(
      (p) => !p.hidden && !ids.has(p.region.id) && p.region.kind !== "area" && !(p.parent && ids.has(p.parent.id)),
    )
    .map((p) => box(p.region.id, p.x, p.y));
  const tol = SNAP_PX / state.view.zoom;
  let bestX: { d: number; at: number; other: (typeof others)[number] } | null = null;
  let bestY: { d: number; at: number; other: (typeof others)[number] } | null = null;
  const mineX = [g.x0, (g.x0 + g.x1) / 2, g.x1];
  const mineY = [g.y0, (g.y0 + g.y1) / 2, g.y1];
  for (const o of others) {
    for (const t of [o.x, o.x + o.w / 2, o.x + o.w])
      for (const m of mineX)
        if (Math.abs(t - m) < tol && (!bestX || Math.abs(t - m) < Math.abs(bestX.d)))
          bestX = { d: t - m, at: t, other: o };
    for (const t of [o.y, o.y + o.h / 2, o.y + o.h])
      for (const m of mineY)
        if (Math.abs(t - m) < tol && (!bestY || Math.abs(t - m) < Math.abs(bestY.d)))
          bestY = { d: t - m, at: t, other: o };
  }
  const lines: { v?: [number, number, number]; h?: [number, number, number] } = {};
  if (bestX) {
    const [sx, sy0] = l.toScene(bestX.at, Math.min(g.y0, bestX.other.y));
    const [, sy1] = l.toScene(bestX.at, Math.max(g.y1, bestX.other.y + bestX.other.h));
    lines.v = [sx, sy0 - 8, sy1 + 8];
  }
  if (bestY) {
    const [sx0, sy] = l.toScene(Math.min(g.x0, bestY.other.x), bestY.at);
    const [sx1] = l.toScene(Math.max(g.x1, bestY.other.x + bestY.other.w), bestY.at);
    lines.h = [sy, sx0 - 8, sx1 + 8];
  }
  setGuides(lines);
  return { dx: bestX ? dx + bestX.d : null, dy: bestY ? dy + bestY.d : null };
}

export function Canvas() {
  const [marquee, setMarquee] = createSignal<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const [panning, setPanning] = createSignal(false);
  let spaceDown = false;
  let sink!: HTMLTextAreaElement;

  // A hidden field at the insertion point receives typing, so dead keys (á, ã, ç),
  // IME composition, dictation and paste all start a region — not just plain keydowns.
  // The welcome screen keeps focus while it is open and hands it back when it closes,
  // so typing starts at the insertion point right after "Blank document".
  createEffect(() => {
    if (state.caret && !state.editing && !welcomeOpen()) sink.focus({ preventScroll: true });
  });

  function onSinkInput(e: InputEvent) {
    if (e.isComposing) return;
    takeSink();
  }

  function takeSink() {
    const text = sink.value;
    sink.value = "";
    if (!text) return;
    if (state.editing) {
      forwardTyping(text);
      return;
    }
    if (!state.caret) return;
    if (text.startsWith('"')) insertAtCaret("text", text.slice(1));
    else insertAtCaret("math", text);
  }

  /** Screen point → scene point (undo pan and zoom). */
  const toScene = (cx: number, cy: number): [number, number] => {
    const rect = viewportEl!.getBoundingClientRect();
    return [(cx - rect.left - state.view.x) / state.view.zoom, (cy - rect.top - state.view.y) / state.view.zoom];
  };

  // canvas view: where each page starts, from the same layout as the page view and the
  // PDF (keep-together pushes included), and the printable column they share
  const pageBreaks = createMemo(() => {
    const l = layout();
    if (l.pageView || !prefs.pageBreaks || !state.doc.page) return [];
    const paged = pagedLayout();
    const starts: number[] = [];
    for (let k = 1; k < paged.pages; k++) {
      const first = paged.list.filter((p) => p.page === k && !p.hidden && !p.parent).sort((a, b) => a.y - b.y)[0];
      if (first) starts.push(l.toScene(0, first.y)[1] - GRID);
    }
    return starts;
  });
  const columnHeight = () => Math.max(contentBottom(), pageBreaks().at(-1) ?? 0) + layout().geom.contentH / 3;

  function startPan(e: PointerEvent) {
    const start = { x: e.clientX, y: e.clientY, vx: state.view.x, vy: state.view.y };
    setPanning(true);
    const move = (ev: PointerEvent) =>
      setState("view", { x: start.vx + ev.clientX - start.x, y: start.vy + ev.clientY - start.y });
    const up = () => {
      setPanning(false);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  // where Tab goes on from when nothing is selected: the region last selected or edited, so
  // Tab after Enter leaves a region moves to the next one rather than to the document's first
  let tabFrom: string | null = null;
  createEffect(() => {
    const at = state.editing ?? state.selection[state.selection.length - 1];
    if (at) tabFrom = at;
  });

  function onBackgroundDown(e: PointerEvent) {
    if (e.button === 1 || spaceDown) {
      e.preventDefault();
      startPan(e);
      return;
    }
    if (e.button !== 0) return;
    commitEditing();
    const start = { x: e.clientX, y: e.clientY };
    let dragging = false;
    const move = (ev: PointerEvent) => {
      if (!dragging && Math.hypot(ev.clientX - start.x, ev.clientY - start.y) > 3) dragging = true;
      if (dragging) setMarquee({ x0: start.x, y0: start.y, x1: ev.clientX, y1: ev.clientY });
    };
    const up = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      const box = marquee();
      setMarquee(null);
      if (!dragging || !box) {
        const [x, y] = layout().fromScene(...toScene(ev.clientX, ev.clientY));
        setState({ caret: [Math.max(0, snap(x)), Math.max(0, snap(y))], selection: [] });
        // a click on the page starts Tab afresh, from the top
        tabFrom = null;
        return;
      }
      const [l, r] = [Math.min(box.x0, box.x1), Math.max(box.x0, box.x1)];
      const [t, b] = [Math.min(box.y0, box.y1), Math.max(box.y0, box.y1)];
      const hit: string[] = [];
      for (const el of viewportEl!.querySelectorAll<HTMLElement>("[data-region]")) {
        const rc = el.getBoundingClientRect();
        if (rc.width && rc.right > l && rc.left < r && rc.bottom > t && rc.top < b) hit.push(el.dataset.region!);
      }
      const ids = hit.filter((id) => layout().byId.get(id)?.region.kind !== "area");
      setState({ selection: e.shiftKey ? [...new Set([...state.selection, ...ids])] : ids, caret: null });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  function onRegionDown(id: string, e: PointerEvent) {
    if (e.button === 1 || spaceDown) {
      e.preventDefault();
      startPan(e);
      return;
    }
    if (e.button !== 0) return;
    e.stopPropagation();
    commitEditing();
    if (e.shiftKey) {
      const sel = state.selection.includes(id) ? state.selection.filter((s) => s !== id) : [...state.selection, id];
      setState({ selection: sel, caret: null });
      return;
    }
    const wasOnlySelection = state.selection.length === 1 && state.selection[0] === id;
    if (!state.selection.includes(id)) setState({ selection: [id], caret: null });
    const origins = originsOf(state.selection);
    const start = { x: e.clientX, y: e.clientY };
    // deltas are measured in document space, so dragging across a page gap works
    const anchor = layout().fromScene(...toScene(e.clientX, e.clientY));
    let moved = false;
    const move = (ev: PointerEvent) => {
      if (!moved && Math.hypot(ev.clientX - start.x, ev.clientY - start.y) < 3) return;
      if (!moved) {
        moved = true;
        checkpoint();
      }
      const [x, y] = layout().fromScene(...toScene(ev.clientX, ev.clientY));
      const raw = [x - anchor[0], y - anchor[1]];
      // edges and centres of other regions win over the grid; ⌘ disables snapping
      const s = ev.metaKey ? { dx: raw[0], dy: raw[1] } : smartSnap(new Set(origins.keys()), origins, raw[0], raw[1]);
      if (ev.metaKey) setGuides({});
      moveTo(origins, Math.round(s.dx ?? snap(raw[0])), Math.round(s.dy ?? snap(raw[1])));
    };
    const up = (ev: PointerEvent) => {
      setGuides({});
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      // a second click on a selected box edits it, with the caret where it was clicked
      if (!moved && wasOnlySelection && ["text", "math"].includes(layout().byId.get(id)?.region.kind ?? "")) {
        startEditing(id, [ev.clientX, ev.clientY]);
      }
      if (moved) {
        regroup([...origins.keys()]);
        setState({ dirty: true, rev: state.rev + 1 });
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY * 0.01));
    else setState("view", { x: state.view.x - e.deltaX, y: state.view.y - e.deltaY });
  }

  /**
   * Whether a key is aimed at the page: typed into the hidden field, on the page itself,
   * or with nothing focused (a click on the page leaves focus on the body). Keys from
   * anywhere else (the sidebar tree, the inspector, a toolbar) belong to what has focus,
   * and must not delete or move regions the user isn't looking at.
   */
  function aimedAtPage(target: EventTarget | null) {
    if (target === sink || target === document.body || target === document.documentElement) return true;
    if (!(target instanceof Element) || !viewportEl!.contains(target)) return false;
    return !target.closest("input, textarea, button, select, [role=dialog], [role=menu]");
  }

  function onKeyDown(e: KeyboardEvent) {
    if (state.editing || !aimedAtPage(e.target)) return;
    const sel = state.selection;
    if (e.key === " " && !e.repeat) {
      spaceDown = true;
      setPanning(true);
      e.preventDefault();
      return;
    }
    if ((e.key === "Backspace" || e.key === "Delete") && sel.length) {
      e.preventDefault();
      deleteRegions(sel);
      return;
    }
    // Tab / ⇧Tab: the next region in reading order; ⌥ + arrow: the nearest one that way
    if (e.key === "Tab" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      const next = stepRegion(sel.length ? sel[sel.length - 1] : tabFrom, e.shiftKey ? -1 : 1);
      if (next) selectInView(next);
      return;
    }
    const way: Record<string, [number, number]> = {
      ArrowLeft: [-1, 0],
      ArrowRight: [1, 0],
      ArrowUp: [0, -1],
      ArrowDown: [0, 1],
    };
    if (e.altKey && !e.metaKey && !e.ctrlKey && way[e.key]) {
      e.preventDefault();
      const next = sel.length ? nearestRegion(sel[sel.length - 1], ...way[e.key]) : stepRegion(null, 1);
      if (next) selectInView(next);
      return;
    }
    if (e.key === "Enter" && sel.length === 1) {
      e.preventDefault();
      if (layout().byId.get(sel[0])?.region.kind === "plot2d") editPlot(sel[0]);
      else startEditing(sel[0]);
      return;
    }
    if (e.key === "Escape") {
      setState({ selection: [], caret: null });
      // leaving the page's selection: Tab starts again from the top
      tabFrom = null;
      return;
    }
    // the context menu from the keyboard: for the selection, or the empty page at the insertion point
    if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
      e.preventDefault();
      const el = sel.length ? viewportEl!.querySelector(`[data-region="${sel[0]}"]`) : sink;
      const box = (el ?? sink).getBoundingClientRect();
      openRegionMenuAt(box.left + 8, box.bottom + 4, sel[0] ?? null);
      return;
    }
    // SMath: on empty space, Enter opens a line and Backspace/Delete close one
    if (state.caret && !sel.length && prefs.shiftContent && !e.metaKey && !e.altKey && !e.ctrlKey) {
      const y = state.caret[1];
      if (e.key === "Enter") {
        e.preventDefault();
        shiftBelow(y, LINE * (e.shiftKey ? 5 : 1));
        return;
      }
      if (e.key === "Backspace" || e.key === "Delete") {
        e.preventDefault();
        shiftBelow(y, -LINE * (e.shiftKey ? 5 : 1));
        return;
      }
    }
    const arrows: Record<string, [number, number]> = {
      ArrowLeft: [-1, 0],
      ArrowRight: [1, 0],
      ArrowUp: [0, -1],
      ArrowDown: [0, 1],
    };
    // ⌘ + arrows resize the selected image or plot: → and ↓ grow it, ← and ↑ shrink it
    const mod = isMac ? e.metaKey : e.ctrlKey;
    if (arrows[e.key] && mod && sel.length === 1 && resizable(layout().byId.get(sel[0])?.region)) {
      e.preventDefault();
      const step = GRID * (e.shiftKey ? 5 : 1);
      const size = resizeBy(sel[0], arrows[e.key][0] * step, arrows[e.key][1] * step);
      if (size) announce(`${size[0]} by ${size[1]}`);
      return;
    }
    if (arrows[e.key]) {
      e.preventDefault();
      if (e.key === "ArrowUp" && !sel.length && !e.shiftKey && returnToLeftBox()) return;
      const step = GRID * (e.shiftKey ? 5 : 1);
      const [dx, dy] = arrows[e.key];
      if (sel.length) nudge(sel, dx * step, dy * step);
      else if (state.caret)
        setState("caret", [Math.max(0, state.caret[0] + dx * step), Math.max(0, state.caret[1] + dy * step)]);
      return;
    }
    // Printable keys fall through to the hidden field (SMath behaviour: typing at the
    // insertion point creates a math region; " starts a text region).
  }

  function onKeyUp(e: KeyboardEvent) {
    if (e.key === " ") {
      spaceDown = false;
      setPanning(false);
    }
  }

  onMount(() => {
    viewportEl!.addEventListener("wheel", onWheel, { passive: false });
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
  });
  onCleanup(() => {
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("keyup", onKeyUp);
  });

  const sheets = createMemo(() => {
    const l = layout();
    if (!l.pageView) return [];
    const byPage: Placed[][] = Array.from({ length: l.pages }, () => []);
    for (const p of l.list) if (!p.hidden) byPage[Math.min(p.page, l.pages - 1)].push(p);
    return byPage;
  });

  // the window's size, for centring pages and fitting the zoom
  onMount(() => {
    const ro = new ResizeObserver(() => setViewportSize({ w: viewportEl!.clientWidth, h: viewportEl!.clientHeight }));
    ro.observe(viewportEl!);
    onCleanup(() => ro.disconnect());
  });
  createEffect(() => {
    const v = state.view;
    const c = clampView(v);
    if (c.x !== v.x || c.y !== v.y) setState("view", { x: c.x, y: c.y });
  });

  // switching between pages and canvas keeps what was in front of you in place; a document
  // just opened starts at its top instead (in the page view, with the whole first page edge)
  let shownDoc = state.doc;
  createEffect(
    on(layout, (l, prev) => {
      const opened = shownDoc !== state.doc;
      shownDoc = state.doc;
      if (!prev || prev.pageView === l.pageView || opened) return;
      const { w, h } = viewportSize();
      const { x, y, zoom } = state.view;
      const doc = prev.fromScene((w / 2 - x) / zoom, (h / 3 - y) / zoom);
      const [sx, sy] = l.toScene(doc[0], doc[1]);
      setState("view", { x: w / 2 - sx * zoom, y: h / 3 - sy * zoom });
    }),
  );

  // page view: "Page 2 of 5" while scrolling
  const [pageHint, setPageHint] = createSignal<string | null>(null);
  let hintTimer: ReturnType<typeof setTimeout> | undefined;
  createEffect(
    on(
      () => [state.view.y, state.view.zoom] as const,
      () => {
        const l = layout();
        const total = l.pages;
        if (!l.pageView || total < 2) return;
        const middle = (viewportSize().h / 2 - state.view.y) / state.view.zoom;
        const k = Math.min(total, Math.max(1, Math.floor(middle / (l.geom.paperH + SHEET_GAP)) + 1));
        setPageHint(`Page ${k} of ${total}`);
        clearTimeout(hintTimer);
        hintTimer = setTimeout(() => setPageHint(null), 1200);
      },
      { defer: true },
    ),
  );

  // the free canvas: a way back after panning off into empty space
  const contentTop = () =>
    Math.min(
      ...layout()
        .list.filter((p) => !p.hidden)
        .map((p) => p.top),
    );
  const lost = createMemo(() => {
    const l = layout();
    const { w, h } = viewportSize();
    if (l.pageView || !w) return false;
    const visible = l.list.filter((p) => !p.hidden);
    if (!visible.length) return false;
    const { x, y, zoom } = state.view;
    const [vx0, vy0, vx1, vy1] = [-x / zoom, -y / zoom, (w - x) / zoom, (h - y) / zoom];
    return !visible.some((p) => {
      const pw = widths[p.region.id] ?? 40;
      const ph = heights[p.region.id] ?? 20;
      return p.left < vx1 && p.left + pw > vx0 && p.top < vy1 && p.top + ph > vy0;
    });
  });
  const backToDocument = () => {
    const z = state.view.zoom;
    setState("view", { x: 48, y: 24 - contentTop() * z });
  };

  /** + New page: a blank page after the last one, with the insertion point at its top. */
  function addPage() {
    const k = layout().pages;
    setState({ pagesWanted: k + 1, selection: [] });
    const l = layout();
    const { margins } = l.geom;
    const stride = l.geom.paperH + SHEET_GAP;
    const [x, y] = l.fromScene(margins.left, k * stride + margins.top);
    setState({ caret: [Math.max(0, snap(x)), Math.max(0, snap(y))] });
    // the new page's top in view
    setState("view", { y: PAGE_PAD - (k * stride - 16) * state.view.zoom });
    sink.focus({ preventScroll: true });
  }

  const caretScene = () => (state.caret ? layout().toScene(state.caret[0], state.caret[1]) : null);

  const gridStyle = () => {
    if (layout().pageView) return {};
    const z = state.view.zoom;
    const step = GRID * 2 * z;
    return {
      "background-size": z < 0.5 ? "0 0" : `${step}px ${step}px`,
      "background-position": `${state.view.x}px ${state.view.y}px`,
    };
  };

  return (
    <div
      ref={viewportEl}
      class="viewport"
      role="main"
      aria-label="Document"
      classList={{
        panning: panning(),
        "page-view": layout().pageView,
        [`pattern-${prefs.canvasPattern}`]: !layout().pageView,
      }}
      style={gridStyle()}
      onPointerDown={onBackgroundDown}
      onScroll={(e) => {
        // focusing an editor can make the browser scroll this (overflow: hidden) box to
        // reveal it; turn that into a normal pan so the scene and the view stay in sync
        const el = e.currentTarget;
        if (!el.scrollLeft && !el.scrollTop) return;
        setState("view", { x: state.view.x - el.scrollLeft, y: state.view.y - el.scrollTop });
        el.scrollLeft = 0;
        el.scrollTop = 0;
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        const [x, y] = layout().fromScene(...toScene(e.clientX, e.clientY));
        setState({ caret: [Math.max(0, snap(x)), Math.max(0, snap(y))], selection: [] });
        openRegionMenu(e, null);
      }}
    >
      <div class="scene" style={sceneStyle()}>
        <Show when={!layout().pageView && prefs.pageBreaks && state.doc.page}>
          <div class="page-column" style={{ width: `${layout().geom.contentW}px`, height: `${columnHeight()}px` }}>
            <span class="page-label" style={{ top: "0px" }}>
              Page 1
            </span>
          </div>
        </Show>
        <For each={pageBreaks()}>
          {(y, i) => (
            <div class="page-break" style={{ top: `${y}px`, width: `${layout().geom.contentW}px` }}>
              <span class="page-label">Page {i() + 2}</span>
            </div>
          )}
        </For>
        <Show
          when={layout().pageView}
          fallback={
            <For each={layout().list.map((p) => p.region)}>
              {(region) => <RegionBox region={region} onDragStart={onRegionDown} />}
            </For>
          }
        >
          <div class="pages" style={{ gap: `${SHEET_GAP}px` }}>
            {/* keyed by page number: a sheet stays mounted while its contents change */}
            <Index each={sheets()}>
              {(regions, k) => <Sheet k={k} regions={regions()} onDragStart={onRegionDown} />}
            </Index>
            <Show when={!state.printing}>
              <button
                class="new-page"
                style={{ width: `${layout().geom.paperW}px` }}
                onPointerDown={(e) => e.stopPropagation()}
                onClick={addPage}
              >
                + New page
              </button>
            </Show>
          </div>
        </Show>
        <Show when={guides().v}>
          {(v) => (
            <div class="guide v" style={{ left: `${v()[0]}px`, top: `${v()[1]}px`, height: `${v()[2] - v()[1]}px` }} />
          )}
        </Show>
        <Show when={guides().h}>
          {(h) => (
            <div class="guide h" style={{ top: `${h()[0]}px`, left: `${h()[1]}px`, width: `${h()[2] - h()[1]}px` }} />
          )}
        </Show>
        <Show when={caretScene()}>
          <div
            class="caret"
            aria-hidden="true"
            style={{ left: `${caretScene()![0]}px`, top: `${caretScene()![1]}px` }}
          />
        </Show>
        <Show when={caretScene() && !state.doc.regions.length && !state.printing}>
          <div
            class="empty-hint"
            aria-hidden="true"
            style={{ left: `${caretScene()![0]}px`, top: `${caretScene()![1]}px` }}
          >
            Click anywhere to start typing
          </div>
        </Show>
        <textarea
          ref={(el) => (sink = sinkEl = el)}
          class="key-sink"
          aria-label="Page: type to insert math, or a quote mark for text"
          autocomplete="off"
          spellcheck={false}
          style={{ left: `${caretScene()?.[0] ?? 0}px`, top: `${caretScene()?.[1] ?? 0}px` }}
          onInput={onSinkInput}
          onCompositionEnd={takeSink}
        />
      </div>
      <Show when={pageHint()}>
        <div class="view-pill page-hint">{pageHint()}</div>
      </Show>
      <Show when={lost()}>
        <button
          class="view-pill back-to-doc"
          aria-label="Back to your document"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={backToDocument}
        >
          ↖ Back to your document
        </button>
      </Show>
      <Show when={marquee()}>
        {(m) => {
          const rect = () => viewportEl!.getBoundingClientRect();
          return (
            <div
              class="marquee"
              style={{
                left: `${Math.min(m().x0, m().x1) - rect().left}px`,
                top: `${Math.min(m().y0, m().y1) - rect().top}px`,
                width: `${Math.abs(m().x1 - m().x0)}px`,
                height: `${Math.abs(m().y1 - m().y0)}px`,
              }}
            />
          );
        }}
      </Show>
    </div>
  );
}
