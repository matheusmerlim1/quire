import { MathEditor, NameNotes } from "./MathEditor";
import type { EditorView } from "prosemirror-view";
import { hitIds } from "./find";
import { trace } from "./trace";
import { fillRefs } from "./xref";
import { type Check, checks, failing, isComparison, mathCheck, regionOf, tableName } from "./checks";
import { captionLabel, defaultLabel, labelKey, labelPattern } from "./labels";
import { openPopover } from "./Popover";
import { Field, Text as InspText } from "./Inspector";
import { DEFAULT_SIZE, editPlot, insertPlot, PlotBody } from "./Plot";
import { editTableAt, isSheet, SheetGrid } from "./TableEditor";
import { prefs } from "./settings";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Index,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
  untrack,
} from "solid-js";
import { produce } from "solid-js/store";
import { api, isMac } from "./api";
import { cellHtml, esc, mathml, prettyValue } from "./math";
import { mathWords } from "./describe";
import { spokenMath } from "./speech";
import { closeMenu, type MenuItems, openRegionMenu, setMenu } from "./menu";
import { createEditor, isLiteral, placeCaret, plainText, releaseEditor } from "./richtext";
import {
  AREA_COLLAPSED_HEIGHT,
  availableWidth,
  checkpoint,
  commitBelow,
  commitEditing,
  convertEditing,
  editClickPoint,
  editText,
  findDraft,
  GRID,
  heights,
  layout,
  localized,
  noteRoom,
  PAGE_WIDTH,
  type Placed,
  regionGrew,
  renameIn,
  revealRegion,
  setHeights,
  setLive,
  setState,
  setWidths,
  startEditing,
  state,
  toggleArea,
  updateDoc,
  updateRegion,
  widths,
  withLocalized,
} from "./store";
import type { Expr, Fix, Region } from "./types";

export interface RegionProps {
  region: Region;
  onDragStart: (id: string, e: PointerEvent) => void;
}

const pt = (n: number | undefined, fallback: number) => `${n ?? fallback}pt`;

/** Apply Djot attributes the HTML renderer leaves as plain attributes ({size=12 font="Arial"}). */
function applyDjotAttributes(el: HTMLElement) {
  for (const node of el.querySelectorAll<HTMLElement>("[size],[font],[color]")) {
    const size = node.getAttribute("size");
    if (size && /^[\d.]+$/.test(size)) node.style.fontSize = `${size}pt`;
    const font = node.getAttribute("font");
    if (font) node.style.fontFamily = `"${font.replace(/"/g, "")}"`;
    const color = node.getAttribute("color");
    if (color && /^#[0-9a-f]{3,8}$/i.test(color)) node.style.color = color;
  }
}

/** Typeset the live inline math placeholders (`$`expr``) inside rendered text. */
function fillInlineMath(el: HTMLElement) {
  for (const span of el.querySelectorAll<HTMLElement>("[data-math]")) {
    const v = state.views[span.dataset.math!];
    span.classList.toggle("inline-error", !!v?.error);
    span.title = v?.error ?? "";
    span.innerHTML = v?.input
      ? mathml(v.input, v.result, v.units, v.constants, v.symbolic)
      : `<code class="raw">?</code>`;
    // read as words, not as the MathML
    if (v?.input) span.setAttribute("aria-label", spokenMath(v.input, v.result, v.units, v.constants, v.symbolic));
    span.setAttribute("role", "img");
  }
}

function Html(props: { html: string | undefined; class?: string }) {
  let el!: HTMLDivElement;
  createEffect(() => {
    el.innerHTML = props.html ?? "";
    applyDjotAttributes(el);
    fillInlineMath(el);
    // this effect also reruns when results arrive, and rewriting the HTML empties the
    // references: fill them again here, without making the text depend on the numbering
    untrack(() => fillRefs(el));
  });
  // references follow the numbering without re-rendering the text
  createEffect(() => {
    void props.html;
    fillRefs(el);
  });
  return <div ref={el} class={props.class} />;
}

function captionPrefix(r: Region, number: string | undefined): string {
  return number ? `${captionLabel(r.kind === "table" ? "table" : "figure", number)} ` : "";
}

/** Double-click a caption's "Figure 3 –": what every figure (or table) caption says. */
function editCaptionLabel(e: MouseEvent, kind: "figure" | "table") {
  const key = labelKey(kind);
  const set = (v: string) =>
    updateDoc((d) => {
      d.styles = { ...d.styles, numbering: { ...d.styles?.numbering, [key]: v || undefined } };
    });
  openPopover(e, kind === "table" ? "Table captions" : "Figure captions", () => (
    <>
      <Field label="Every caption reads" hint="{n} is the number, e.g. Figura {n} –">
        <InspText value={labelPattern(kind)} placeholder={defaultLabel(kind)} onCommit={set} />
      </Field>
      <div class="insp-hint">Empty follows the document's language. Also in Document ▸ Numbering.</div>
    </>
  ));
}

/** A caption to start editing as soon as it appears (right after Add Caption). */
export const [captionToEdit, setCaptionToEdit] = createSignal<string | null>(null);

function Caption(props: { p: Placed }) {
  const r = () => props.p.region;
  const [editing, setEditing] = createSignal(false);
  createEffect(() => {
    if (captionToEdit() === r().id && r().caption) {
      setCaptionToEdit(null);
      setEditing(true);
    }
  });
  const lang = () => state.doc.meta?.lang;
  const commit = (text: string) => {
    setEditing(false);
    const caption = r().caption;
    if (!caption || text === localized(caption.text, lang())) return;
    // a caption in several languages keeps the others
    updateRegion(r().id, { caption: { ...caption, text: withLocalized(caption.text, text, lang()) } });
  };
  return (
    <Show when={r().caption}>
      <div class="caption">
        <span
          class="caption-num"
          title="Double-click to rename all"
          onDblClick={(e) => editCaptionLabel(e, r().kind === "table" ? "table" : "figure")}
        >
          {captionPrefix(r(), props.p.number)}
        </span>
        <Show
          when={editing()}
          fallback={
            <span
              class="caption-edit-target"
              title="Double-click to edit"
              onDblClick={(e) => {
                e.stopPropagation();
                setEditing(true);
              }}
            >
              <Html html={state.report?.html[`${r().id}:caption`]} class="caption-text" />
            </span>
          }
        >
          <input
            class="caption-input"
            value={localized(r().caption!.text, lang())}
            size={Math.max(12, localized(r().caption!.text, lang()).length + 2)}
            spellcheck={true}
            aria-label="Caption"
            // after the menu that opened it has closed and handed focus back to the page
            ref={(el) => setTimeout(() => (el.focus(), el.select()), 60)}
            onPointerDown={(e) => e.stopPropagation()}
            onDblClick={(e) => e.stopPropagation()}
            onInput={(e) => (e.currentTarget.size = Math.max(12, e.currentTarget.value.length + 2))}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") commit(e.currentTarget.value.trim());
              else if (e.key === "Escape") setEditing(false);
            }}
            onBlur={(e) => editing() && commit(e.currentTarget.value.trim())}
          />
        </Show>
      </div>
    </Show>
  );
}

function RichTextEditor(props: { p: Placed; style: Record<string, string | undefined> }) {
  let host!: HTMLDivElement;
  let view: EditorView | undefined;
  const id = untrack(() => props.p.region.id);
  onCleanup(() => view && releaseEditor(view));
  onMount(async () => {
    const src = untrack(() => editText(props.p.region));
    // plain text (e.g. just switched from math) opens instantly; markup goes through the parser
    const plain = !/[\\*_[\]{}`~^$<>#|-]|^\s*\d+\./.test(src) && isLiteral(src);
    const html = plain ? null : await api.djotHtml(src).catch(() => "");
    if (state.editing !== id) return;
    view = createEditor(host, html ?? { text: src }, {
      onChange: (djot) => setLive(id, djot),
      onExit: (below) => (below ? commitBelow(id) : commitEditing()),
      onKey: (v, e) => {
        // "Area" + space became text, then ":" — it was a definition after all
        if (state.editingFresh && (e.key === ":" || e.key === "=") && !e.metaKey) {
          const word = plainText(v).trim();
          if (/^[\p{L}][\p{L}\p{N}_.]*$/u.test(word)) {
            e.preventDefault();
            convertEditing("math", `${word} ${e.key === ":" ? ":=" : "="}`);
            return true;
          }
        }
        return false;
      },
    });
    if (editClickPoint) placeCaret(view, ...editClickPoint);
  });
  return <div ref={host} class="text-body rich-host" style={props.style} />;
}

/** The engine's repair for an error, one click away. */
function FixButton(props: { id: string; fix: Fix }) {
  const label = () => (props.fix.kind === "rename" ? `Use ${props.fix.to}` : "Show definition");
  return (
    <button
      class="err-fix"
      onPointerDown={(e) => e.stopPropagation()}
      onClick={(e) => {
        e.stopPropagation();
        const f = props.fix;
        if (f.kind === "rename") renameIn(props.id, f.from, f.to);
        else revealRegion(f.id);
      }}
    >
      {label()}
    </button>
  );
}

/** `noteWidth`: the room for the error note (see noteRoom). */
function MathBody(props: { r: Region; noteWidth: () => number }) {
  const view = () => state.views[props.r.id];
  const editing = () => state.editing === props.r.id;
  // a check: a comparison reads ✓ / ✗ instead of = 1 / = 0; an OK / FAIL result is coloured
  const check = () => mathCheck(props.r, view());
  const comparison = () => check() !== null && isComparison(view()?.input);
  const rendered = () => {
    const v = view();
    const result = props.r.show && !comparison() ? v?.result : null;
    return v?.input ? mathml(v.input, result, v.units, v.constants, props.r.show === "symbolic") : null;
  };
  return (
    <>
      <Show when={editing()}>
        <MathEditor r={props.r} />
      </Show>
      <div
        class="math-line"
        classList={{ preview: editing(), "check-pass": check() === true, "check-fail": check() === false }}
        aria-hidden={editing() || undefined}
      >
        <Show when={rendered()} fallback={<code class="raw">{props.r.expr || " "}</code>}>
          {/* screen readers get the formula in words; the MathML would be read symbol by symbol */}
          <span class="sr-only">{mathWords(props.r.id, false)}</span>
          <span class="m" innerHTML={rendered()!} aria-hidden="true" />
        </Show>
        <Show when={comparison()}>
          <span
            class="check-badge"
            aria-hidden="true"
            classList={{ pass: check() === true, fail: check() === false }}
            title={check() ? "This check passes" : "This check fails"}
          >
            {check() ? "✓" : "✗ fails"}
          </span>
        </Show>
        <Show when={props.r.note && !editing()}>
          <Html html={state.report?.html[`${props.r.id}:note`]} class="note" />
        </Show>
      </div>
      <Show when={editing() && prefs.nameNotes}>
        <NameNotes id={props.r.id} />
      </Show>
      <Show when={view()?.error}>
        <div class="err" style={{ "max-width": `${props.noteWidth()}px` }}>
          <span class="err-text">{view()!.error}</span>
          <Show when={!editing() && view()?.fix}>{(fix) => <FixButton id={props.r.id} fix={fix()} />}</Show>
        </div>
      </Show>
    </>
  );
}

function TextBody(props: { p: Placed }) {
  const r = () => props.p.region;
  const style = () => ({
    "font-family": r().style?.font ?? state.doc.styles?.body?.font,
    "font-size": pt(r().style?.size, state.doc.styles?.body?.size ?? 10),
    // aligned text needs the full width to align within; plain text hugs its content
    width: r().width
      ? `${r().width}px`
      : /\{[^}\n]*\.(center|right|justify)\b/.test(localized(r().text, state.doc.meta?.lang))
        ? `${availableWidth(props.p.x)}px`
        : undefined,
    // unsized text wraps at the page edge
    "max-width": r().width ? undefined : `${availableWidth(props.p.x)}px`,
    "--num": props.p.number && state.doc.styles?.numbering?.headings !== "" ? `"${props.p.number}. "` : undefined,
  });
  return (
    <Show
      when={state.editing !== r().id}
      fallback={<RichTextEditor p={props.p} style={{ ...style(), "min-width": "160px", "white-space": "normal" }} />}
    >
      <div style={style()} class="text-wrap" classList={{ fixed: !!r().width }}>
        <Show
          when={state.report?.html[r().id] !== undefined}
          fallback={<div class="text-body">{localized(r().text, state.doc.meta?.lang)}</div>}
        >
          <Html html={state.report?.html[r().id]} class="text-body" />
        </Show>
      </div>
    </Show>
  );
}

function ImageBody(props: { p: Placed }) {
  const r = () => props.p.region;
  const size = () => r().size ?? [200, 150];
  const url = () => (r().src ? state.assets[r().src!] : undefined);
  return (
    <figure class="figure" style={{ width: `${size()[0]}px` }}>
      <Show when={r().caption?.position === "above"}>
        <Caption p={props.p} />
      </Show>
      <Show
        when={url()}
        fallback={<div class="img-missing" style={{ width: `${size()[0]}px`, height: `${size()[1]}px` }} />}
      >
        <img src={url()} width={size()[0]} height={size()[1]} draggable={false} alt={r().caption ? "" : "Image"} />
      </Show>
      <Show when={r().caption?.position !== "above"}>
        <Caption p={props.p} />
      </Show>
    </figure>
  );
}

/** A plot as a figure: captioned like an image, with its error below. */
function PlotFigure(props: { p: Placed }) {
  const r = () => props.p.region;
  const error = () => state.views[r().id]?.error;
  return (
    <figure class="figure plot-figure" style={{ width: `${(r().size ?? DEFAULT_SIZE)[0]}px` }}>
      <Show when={r().caption?.position === "above"}>
        <Caption p={props.p} />
      </Show>
      <PlotBody r={r()} />
      <Show when={error()}>
        <div class="err" title={error()!}>
          <span class="err-text">{error()}</span>
        </div>
      </Show>
      <Show when={r().caption?.position !== "above"}>
        <Caption p={props.p} />
      </Show>
    </figure>
  );
}

function AreaBody(props: { p: Placed }) {
  const r = () => props.p.region;
  const height = () => (r().collapsed ? AREA_COLLAPSED_HEIGHT : (r().height ?? 90));
  return (
    <div
      class="area"
      classList={{ collapsed: !!r().collapsed }}
      style={{ width: `${layout().pageView ? layout().geom.contentW : PAGE_WIDTH}px`, height: `${height()}px` }}
    >
      <div class="area-header">
        <button
          class="disclosure"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={() => toggleArea(r().id)}
          title={r().collapsed ? "Expand" : "Collapse"}
          aria-label={r().collapsed ? "Expand area" : "Collapse area"}
          aria-expanded={!r().collapsed}
        >
          {r().collapsed ? "▸" : "▾"}
        </button>
        <Html html={state.report?.html[`${r().id}:title`]} class="area-title" />
      </div>
    </div>
  );
}

function TableBody(props: { p: Placed }) {
  const r = () => props.p.region;
  const view = () => state.views[r().id];
  const rows = (): Expr[][] | null => {
    const res = view()?.result;
    return res?.t === "Matrix" ? res.c : null;
  };
  const BoundTable = () => (
    <Show
      when={rows()}
      fallback={
        <div class="placeholder-box" style={{ width: `${r().size?.[0] ?? 300}px` }}>
          Table of <code>{r().source ?? "—"}</code>
          <Show when={view()?.error}>
            <div class="err">{view()!.error}</div>
          </Show>
        </div>
      }
    >
      <table class="table" classList={{ "head-row": !!r().header?.row, "head-col": !!r().header?.column }}>
        <tbody>
          <For each={rows()!}>
            {(row) => (
              <tr>
                <For each={row}>{(cell) => <td innerHTML={cellHtml(cell)} />}</For>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </Show>
  );
  return (
    <figure class="figure">
      <Show when={r().caption?.position === "above"}>
        <Caption p={props.p} />
      </Show>
      <Show when={isSheet(r())} fallback={BoundTable()}>
        <SheetGrid p={props.p} />
      </Show>
      <Show when={r().caption?.position !== "above"}>
        <Caption p={props.p} />
      </Show>
    </figure>
  );
}

const textOf = (html: string | undefined) =>
  new DOMParser().parseFromString(html ?? "", "text/html").body.textContent?.trim() ?? "";

/** The text of the first heading in rendered text (a region may hold more after it). */
const headingOf = (html: string | undefined) => {
  const doc = new DOMParser().parseFromString(html ?? "", "text/html");
  return (doc.querySelector("h1, h2, h3, h4, h5, h6") ?? doc.body).textContent?.trim() ?? "";
};

/** Insert ▸ Summary of Checks: every check, ✓ or ✗, kept current. */
function ChecksBody(props: { p: Placed }) {
  const r = () => props.p.region;
  const start = () => (state.doc.page?.pageNumber as { start?: number } | undefined)?.start ?? 1;
  const summary = () => {
    const f = failing().length;
    const n = checks().length;
    return f ? `${f} of ${n} failing` : n === 1 ? "1 check, passing" : `All ${n} checks pass`;
  };
  // rows stay in place as the layout changes; each one updates only what changed in it
  return (
    <div class="contents checks-summary" style={{ width: `${Math.min(availableWidth(props.p.x), 640)}px` }}>
      <div class="contents-title">{String(r().title ?? "Summary of Checks")}</div>
      <Show
        when={checks().length}
        fallback={<div class="contents-empty">No checks yet: show a comparison with =, like σ ≤ f.y =.</div>}
      >
        <div class="checks-total" classList={{ failing: failing().length > 0 }}>
          {summary()}
        </div>
        <Index each={checks()}>{(c) => <CheckRow c={c()} start={start()} />}</Index>
      </Show>
    </div>
  );
}

function CheckRow(props: { c: Check; start: number }) {
  // regions keep their identity across layouts, so this changes only when the row shows another region
  const region = createMemo(() => regionOf(props.c.id));
  const label = createMemo(() => {
    const c = props.c;
    const v = state.views[c.id];
    const r = region();
    if (c.cell && r) return esc(`${tableName(r)} · ${c.row || `row ${c.cell[0] + 1}`}`);
    // the comparison itself, typeset, and its note if it has one
    const input = v?.input?.t === "Define" ? v.input.c.rhs : v?.input;
    const math = input ? mathml(input, null, v!.units, v!.constants) : esc(r?.expr ?? "");
    const note = r?.note ? ` <span class="checks-note">${esc(String(r.note))}</span>` : "";
    return math + note;
  });
  return (
    <div
      class="contents-row"
      data-ref={props.c.id}
      onPointerDown={(ev) => ev.stopPropagation()}
      onClick={() => revealRegion(props.c.id)}
      title="Go to"
    >
      <span class="checks-mark" classList={{ pass: props.c.pass, fail: !props.c.pass }}>
        {props.c.pass ? "✓" : "✗"}
      </span>
      <span class="contents-label" innerHTML={label()} />
      <span class="contents-dots" />
      <Show when={layout().pageView}>
        <span class="contents-page">{props.c.page + props.start}</span>
      </Show>
    </div>
  );
}

interface ContentsEntry {
  id: string;
  number: string;
  label: string;
  level: number;
  page: number;
}

const sameEntries = (a: ContentsEntry[], b: ContentsEntry[]) =>
  a.length === b.length &&
  a.every(
    (e, i) =>
      e.id === b[i].id &&
      e.number === b[i].number &&
      e.label === b[i].label &&
      e.level === b[i].level &&
      e.page === b[i].page,
  );

/** Table of contents / list of figures / list of tables — always current. */
function ContentsBody(props: { p: Placed }) {
  const r = () => props.p.region;
  const of = () => (r().of as string | undefined) ?? "headings";
  const start = () => (state.doc.page?.pageNumber as { start?: number } | undefined)?.start ?? 1;
  // the layout changes with every height measured: parse a heading's HTML only when it changes
  let parsed = new Map<string, string>();
  const entries = createMemo(
    (): ContentsEntry[] => {
      const headings = of() === "headings";
      const seen = new Map<string, string>();
      const text = (html: string | undefined) => {
        const key = `${headings ? "h" : "c"}:${html ?? ""}`;
        const t = parsed.get(key) ?? (headings ? headingOf(html) : textOf(html));
        seen.set(key, t);
        return t;
      };
      const out = layout()
        .list.filter((q) => !q.hidden && q.number)
        .filter((q) =>
          headings
            ? q.region.kind === "text"
            : of() === "tables"
              ? q.region.kind === "table" && !!q.region.caption
              : q.region.kind !== "table" && q.region.kind !== "text" && !!q.region.caption,
        )
        .map((q) => {
          const html = state.report?.html[headings ? q.region.id : `${q.region.id}:caption`];
          return {
            id: q.region.id,
            number: headings ? q.number! : "",
            label: headings ? text(html) : `${captionPrefix(q.region, q.number)}${text(html)}`,
            level: headings ? q.number!.split(".").length : 1,
            page: q.page,
          };
        });
      parsed = seen;
      return out;
    },
    [],
    { equals: sameEntries },
  );
  // rows stay in place as the layout changes; each one updates only what changed in it
  return (
    <div class="contents" style={{ width: `${Math.min(availableWidth(props.p.x), 640)}px` }}>
      <div class="contents-title">{String(r().title ?? "Contents")}</div>
      <Show when={entries().length} fallback={<div class="contents-empty">Nothing to list yet.</div>}>
        <Index each={entries()}>
          {(e) => (
            <div
              class={`contents-row lvl-${e().level}`}
              data-ref={e().id}
              onPointerDown={(ev) => ev.stopPropagation()}
              onClick={() => revealRegion(e().id)}
              title="Go to"
            >
              <Show when={e().number}>
                <span class="contents-num">{e().number}</span>
              </Show>
              <span class="contents-label">{e().label}</span>
              <span class="contents-dots" />
              <Show when={layout().pageView}>
                <span class="contents-page">{e().page + start()}</span>
              </Show>
            </div>
          )}
        </Index>
      </Show>
    </div>
  );
}

/** Drag handle that sets a text region's width or an image's size (Shift: free aspect). */
/** The size an image or plot is shown at, `size` or the default its body falls back to. */
function shownSize(r: Region): [number, number] {
  return r.size ?? (r.kind === "plot2d" ? DEFAULT_SIZE : [200, 150]);
}

/** Whether a region is resized as a box (image, plot) rather than by its width (text). */
export const resizable = (r: Region | undefined) => r?.kind === "image" || r?.kind === "plot2d";

/**
 * Resize an image or plot from the keyboard by `dw` and `dh`, one undo step. An image
 * keeps its proportions, as it does when dragged. Returns the new size.
 */
export function resizeBy(id: string, dw: number, dh: number): [number, number] | null {
  const r = layout().byId.get(id)?.region;
  if (!r || !resizable(r)) return null;
  const [w0, h0] = shownSize(r);
  let size: [number, number];
  if (r.kind === "image") {
    const w = Math.max(24, w0 + (dw || dh));
    size = [w, Math.max(16, Math.round((w * h0) / w0))];
  } else size = [Math.max(160, w0 + dw), Math.max(120, h0 + dh)];
  if (size[0] !== w0 || size[1] !== h0) updateRegion(id, { size });
  return size;
}

function ResizeHandle(props: { p: Placed }) {
  const r = () => props.p.region;
  const onDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    const id = r().id;
    const startX = e.clientX;
    const startY = e.clientY;
    const sized = resizable(r());
    const [w0, h0] = sized ? shownSize(r()) : [r().width ?? widths[id] ?? 200, 0];
    const ratio = h0 / w0;
    // the undo step is taken on the first move, as when dragging a region: a click that
    // doesn't drag changes nothing, so it mustn't mark the document edited or clear Redo
    let moved = false;
    const move = (ev: PointerEvent) => {
      if (!moved && Math.hypot(ev.clientX - startX, ev.clientY - startY) < 3) return;
      if (!moved) {
        moved = true;
        checkpoint();
      }
      const w = Math.max(24, Math.round((w0 + (ev.clientX - startX) / state.view.zoom) / GRID) * GRID);
      const free = Math.max(16, Math.round(h0 + (ev.clientY - startY) / state.view.zoom));
      setState(
        "doc",
        produce((d) => {
          const reg = findDraft(d, id);
          if (!reg) return;
          if (reg.kind === "image") reg.size = [w, ev.shiftKey ? free : Math.round(w * ratio)];
          else if (reg.kind === "plot2d") reg.size = [Math.max(160, w), Math.max(120, free)];
          else reg.width = w;
        }),
      );
    };
    // a cancelled pointer (the system took it over) keeps the size reached so far, like a release
    const end = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      if (moved) setState({ dirty: true, rev: state.rev + 1 });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
  };
  return (
    <div
      class="resize-handle"
      classList={{ corner: resizable(r()) }}
      onPointerDown={onDown}
      title={resizable(r()) ? `Drag to resize (${isMac ? "⌘" : "Ctrl+"}Arrows)` : "Drag to resize"}
    />
  );
}

/** The units a result can be shown in, as a menu at the clicked unit. */
function openUnitMenu(e: MouseEvent, r: Region) {
  const choices = state.views[r.id]?.unitChoices ?? [];
  const current = r.display?.unit;
  const setUnit = (unit: string | undefined) => {
    closeMenu();
    updateRegion(r.id, (d) => {
      d.display = { ...d.display, unit };
      if (!unit) delete d.display.unit;
      if (!Object.keys(d.display).length) delete d.display;
    });
  };
  const items: MenuItems = choices.map((c) => ({
    label: prettyValue(c.text),
    checked: c.unit === current,
    run: () => setUnit(c.unit),
  }));
  items.push(null, { label: "Automatic", checked: !current, run: () => setUnit(undefined) });
  items.push({
    label: "Other Unit…",
    run: () => {
      closeMenu();
      startEditing(r.id);
    },
  });
  const box = (e.target as Element).closest(".res")!.getBoundingClientRect();
  setMenu({ x: box.left, y: box.bottom + 4, items });
}

const KIND_LABEL: Record<string, string> = {
  plot3d: "3D plot (not supported in this version yet)",
  data: "Data import (not supported in this version yet)",
  legacy: "Unconverted SMath region",
  // held by the engine when it can't read a region (a newer Quire's); saved back as it was
  unreadable: "A region this version of Quire can't read, kept as it is",
  pagebreak: "Page break",
};

export function RegionBox(props: RegionProps) {
  // keyed by the region (a stable store object); its placement is looked up reactively,
  // so layout changes move the element instead of rebuilding it (and losing focus)
  const placed = (): Placed =>
    layout().byId.get(props.region.id) ?? {
      region: props.region,
      x: 0,
      y: 0,
      shift: 0,
      hidden: true,
      parent: null,
      page: 0,
      left: 0,
      top: 0,
    };
  const r = () => props.region;
  const selected = () => state.selection.includes(r().id);
  const editing = () => state.editing === r().id;
  let el!: HTMLDivElement;
  // pagination needs real heights; report them whenever the rendering changes size
  onMount(() => {
    const ro = new ResizeObserver(() => {
      const h = el.offsetHeight;
      const before = untrack(() => heights[r().id]) ?? 0;
      if (h && Math.abs(before - h) > 0.5) {
        setHeights(r().id, h);
        regionGrew(r().id, before, h);
      }
      const w = el.offsetWidth;
      if (w && Math.abs((untrack(() => widths[r().id]) ?? 0) - w) > 0.5) setWidths(r().id, w);
    });
    ro.observe(el);
    onCleanup(() => ro.disconnect());
  });
  return (
    <div
      ref={el}
      class={`region ${r().kind}`}
      classList={{
        selected: selected(),
        editing: editing(),
        error: !!state.views[r().id]?.error,
        found: hitIds().has(r().id),
        "trace-def": !!trace()?.defs.has(r().id),
        "trace-use": !!trace()?.uses.has(r().id) && !trace()?.defs.has(r().id),
      }}
      data-region={r().id}
      style={{
        left: `${placed().left}px`,
        top: `${placed().top}px`,
        display: placed().hidden ? "none" : undefined,
        "font-size": r().kind === "math" ? pt(r().style?.size, state.doc.styles?.math?.size ?? 10) : undefined,
      }}
      onPointerDown={(e) => {
        // inside the region being edited, clicks belong to the editor, not the canvas
        if (editing()) return e.stopPropagation();
        // a result's unit: pick another one
        if (
          e.button === 0 &&
          !e.shiftKey &&
          (e.target as Element).closest?.(".res .u") &&
          state.views[r().id]?.unitChoices?.length
        ) {
          e.stopPropagation();
          e.preventDefault();
          setState({ selection: [r().id], caret: null });
          return openUnitMenu(e, r());
        }
        props.onDragStart(r().id, e);
      }}
      onDblClick={(e) =>
        r().kind === "table"
          ? editTableAt(r().id, [e.clientX, e.clientY])
          : r().kind === "plot2d"
            ? editPlot(r().id)
            : startEditing(r().id, [e.clientX, e.clientY])
      }
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation(); // the canvas has its own menu
        if (!selected()) setState({ selection: [r().id], caret: null });
        openRegionMenu(e, r().id);
      }}
    >
      <Switch
        fallback={
          <div
            class="placeholder-box"
            style={{ width: `${r().size?.[0] ?? 240}px`, height: `${r().size?.[1] ?? 60}px` }}
          >
            {KIND_LABEL[r().kind] ?? r().kind}
          </div>
        }
      >
        <Match when={r().kind === "math"}>
          <MathBody r={r()} noteWidth={() => noteRoom(placed(), layout().list, heights, availableWidth(placed().x))} />
        </Match>
        <Match when={r().kind === "text"}>
          <TextBody p={placed()} />
        </Match>
        <Match when={r().kind === "image"}>
          <ImageBody p={placed()} />
        </Match>
        <Match when={r().kind === "area"}>
          <AreaBody p={placed()} />
        </Match>
        <Match when={r().kind === "table"}>
          <TableBody p={placed()} />
        </Match>
        <Match when={r().kind === "toc" && r().of === "checks"}>
          <ChecksBody p={placed()} />
        </Match>
        <Match when={r().kind === "toc"}>
          <ContentsBody p={placed()} />
        </Match>
        <Match when={r().kind === "plot2d"}>
          <PlotFigure p={placed()} />
        </Match>
      </Switch>
      <Show
        when={
          selected() &&
          state.selection.length === 1 &&
          !editing() &&
          !state.printing &&
          ["text", "image", "plot2d"].includes(r().kind)
        }
      >
        <ResizeHandle p={placed()} />
      </Show>
      {/* a function definition can be plotted in one click (shown on hover or when selected) */}
      <Show
        when={r().kind === "math" && !editing() && !state.printing && /\(/.test(state.views[r().id]?.defines ?? "")}
      >
        <button
          class="plot-this"
          title="Plot this function below it"
          aria-label="Plot this function"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            setState({ selection: [r().id], caret: null });
            insertPlot();
          }}
        >
          <span>∿ Plot</span>
        </button>
      </Show>
    </div>
  );
}
