# 03 — Code Map

Where things are today and how they fit together. [01-architecture.md](01-architecture.md) records why the project is built this way; this page describes what is actually there. When you move or split a file, update this page in the same change.

## 1. The big picture

```text
                ┌──────────────────────── app/ (GPL-3.0-or-later) ────────────────────────┐
                │  app/src: SolidJS UI                    app/src-tauri: desktop shell     │
 keystroke ───▶ │  store.ts ─ mutate ─▶ App.tsx ─ report ─▶ api.ts ──invoke──▶ main.rs     │
                │      ▲                                     │ (browser: fetch /api)  │      │
                │      └──────── showReport ◀── Report ──────┘                        │      │
                └─────────────────────────────────────────────────────────────────────┼──────┘
                                                                                      ▼
                ┌──────────────────────── crates/ (MIT OR Apache-2.0) ─────────────────────┐
                │  quire-service: one function per editor call, JSON in and out            │
                │      ├─ quire-doc     document model, .quire / .quirez on disk           │
                │      ├─ quire-smath   SMath .sm worksheets → a document (import only)    │
                │      └─ quire-eval    worksheet evaluation, tables, plots, symbolic math │
                │            ├─ quire-ast    linear math syntax: lex, parse, print         │
                │            └─ quire-units  dimensions and the unit table                 │
                │  quire-cli: the `quire` command, and `quire serve` (the same calls over  │
                │             localhost HTTP, so the UI runs in a plain browser)           │
                │  quire-web: the same calls compiled to WebAssembly (the web version)     │
                └──────────────────────────────────────────────────────────────────────────┘
```

Three rules hold the layers apart:

- **The app holds no maths.** It keeps the document as the JSON that is on disk, asks the backend for an evaluated `Report` and draws it. Anything that needs units, parsing or numbers is asked of `quire-service`.
- **The engine holds no UI.** No crate under `crates/` knows about windows, the DOM or Tauri. `quire-service` is the only API the app sees.
- **The engine stays permissive.** Crates under `crates/` are MIT OR Apache-2.0 and never depend on GPL code; `app/` is GPL-3.0-or-later.

## 2. One edit, start to finish

1. A keystroke reaches an editor component (`MathEditor.tsx`, the ProseMirror view in `richtext.ts`, `TableEditor.tsx`) or a command (`App.tsx` `runCommand`).
2. The change goes through `mutate()` in `store.ts`, which records an undo step (bounded by size, see `History` there) and marks the document dirty.
3. An effect in `App.tsx` (`liveReport`) sends the whole document to `api.report`. Requests are numbered when they are scheduled (`latest.ts`), so a slow answer never overwrites a newer edit or another document.
4. `api.ts` calls the Tauri command `report` in the desktop app, the WebAssembly engine in the web version (`engine.ts`), or `POST /api/report` against `quire serve` while developing in a browser.
5. `quire_service::report` parses the JSON into a `quire_doc::Document`, turns its regions into `MathInput`s in reading order (`math_inputs`), runs `quire_eval::Worksheet` and returns a `Report`: per region the parsed AST, the result, errors, fixes, unit choices, table cells and plot samples.
6. `showReport` in `store.ts` stores it. Errors on the line being typed wait until the line is finished ("errors wait while you type").
7. `Region.tsx` re-renders what changed: math through `math.ts` (AST → MathML), words for screen readers through `speech.ts`, tables through `TableEditor.tsx`, plots through `Plot.tsx`.

A whole document re-evaluates in well under a millisecond, so there is no incremental evaluation and no cache to keep in sync.

Saving is the same path in reverse: `api.save` → `quire_service::save`, which writes canonical JSON (`canonical.rs`) and moves staged images into the bundle (`assets.rs`). In a notebook, `notebook.ts` saves by itself (`flushSave`, `installAutosave`).

## 3. Engine crates

| Crate | Files | What lives there |
|---|---|---|
| `quire-ast` | `lexer.rs`, `parser.rs`, `printer.rs`, `ast.rs` | The linear syntax ([02-file-format.md §4.3](02-file-format.md)). `print(parse(s))` is canonical; parentheses the user typed survive as `Expr::Paren`. Precedence is listed at the top of `parser.rs`. Trees deeper than `MAX_DEPTH` (200, as the SMath importer) are refused, so every recursive walk over a parsed tree fits a small stack. |
| `quire-units` | `db.rs`, `dim.rs`, `rational.rs` | `Dim` (seven SI exponents, rational) and the built-in unit table with factors from NIST SP 811, plus SMath Studio's units and constants (quoted only; `lookup_bare` leaves them out for bare names). `quire-eval/tests/smath_units.rs` checks every SMath unit name. |
| `quire-eval` | `value.rs`, `ops.rs` | Values (SI magnitude + dimension, matrices, strings) and arithmetic with dimension checks. |
| | `interp.rs` | Evaluating one expression in a scope; the lazy constructs (`if`, `for`, `sum`, `int`, `diff`, …) and user functions. |
| | `builtins.rs`, `stats.rs`, `solvers.rs`, `numeric.rs`, `linalg.rs`, `special.rs` | Built-in functions whose arguments are evaluated first; statistics and fitting; ODE solvers, `linfit` and nonlinear `solve`; plain linear algebra; SMath's matrix functions (`col`, `csort`, `tr`, `polyroots`, `submatrix`, `minor`, `rank`, `findrows`…); SMath's other built-ins (`Gamma`, `erf`, `strlen`, `UnitsOf`, `error`…) and the messages for SMath names Quire doesn't have (`random`, files) or that aren't SMath's (`gcd`, `last`). |
| | `budget.rs` | Limits on one evaluation: the step budget (which matrix operations charge too) and the largest matrix. With the nesting limits in `interp.rs` and `symbolic/work.rs`, any document gives an error, never a crash or a hang; its tests try each limit on a 1 MB stack, as in WebAssembly. |
| | `constants.rs` | Built-in constants and the name-resolution order (your variable, then constant, then unit). |
| | `worksheet.rs` | Worksheet semantics: definitions in reading order, global `≡`, "defined further down", working units. |
| | `format.rs` | Result display: rounding, notation, which unit a result is shown in. |
| | `sheet/` | Spreadsheet tables: `formula.rs` (A1 syntax), `functions.rs` (SUM, IF, VLOOKUP…), `mod.rs` (evaluating a table, named tables as matrices). |
| | `plot.rs` | Sampling plot series where the plot sits in reading order; SMath systems (`sys`), marker rows and animation frames. |
| | `symbolic/` | `→`: `mod.rs` (the expression tree and rewriting), `simplify.rs`, `poly.rs` (factoring), `solve.rs`, `integrate.rs`, `limit.rs` (limits and series), `work.rs` (the work budget and depth limits), `tests.rs`. `bridge.rs`: formulas as values (`Value::Sym`) in SMath documents, where a name with no value is a symbol. |
| `quire-doc` | `lib.rs`, `lenient.rs`, `single.rs` | The document model; unknown keys are kept in `extra` maps. `lenient.rs` reads what it can of values it doesn't expect, keeps them as written, and turns regions it can't read into placeholders (held for the editor on open, put back on save). `.quirez` (one ZIP file) in `single.rs`. |
| `quire-service` | `lib.rs` | `report`, `open`, `save`, `new_document`, `library` (units and constants for the galleries). |
| | `canonical.rs` | Canonical `document.json` output: key order and line breaks are fixed, so diffs stay small. |
| | `assets.rs` | Images, content-addressed (`assets/<sha256:16>.<ext>`), staged in memory until the first save. |
| | `text.rs` | Djot → HTML for text regions; documents are untrusted, so raw HTML is dropped. |
| | `templates.rs`, `templates/*.json` | Built-in templates and the user's own. |
| | `notebook.rs` | Notebooks: list, create, rename, move, duplicate, trash, style, search. One entry point, `call(op, args)`. |
| `quire-web` | `lib.rs` | The service as WebAssembly for the web version: `dispatch` (the calls a browser can make, files as base64) behind a plain `alloc`/`call` interface, no binding generator. Built with `npm --prefix app run wasm` (the `web` profile in `Cargo.toml`, optimised for size). |
| `quire-smath` | `lib.rs` | `convert` and `convert_file` (the file name sets the document's source): XML in, `Imported` out (document, cached results, assets, warnings). |
| | `convert.rs` | The importer proper: settings, metadata, page model, and every region kind. |
| | `math.rs` | SMath's RPN math tokens → AST → Quire's linear syntax. |
| | `text.rs` | SMath's XHTML-like paragraphs → Djot. |
| `quire-cli` | `main.rs`, `lib.rs` | `quire calc`, `quire eval`, `quire fmt --check`, `quire check` (compare with `results.json`), `quire import <file.sm> [out.quire]`. |
| | `serve.rs` | `quire serve`: the service over `127.0.0.1` for UI work. Development only; it checks the Host header and a custom request header. |
| | `tests/` | Fixtures with independently known results; see [05-testing.md](05-testing.md). |

**Opening a file.** `quire_service::open` reads a `.quire`, `.quirez` or `.sm`. An `.sm` goes through `quire-smath` and comes back as an unsaved document: `open_smath` stages its images, and Save asks for a new file, so the `.sm` is never written. The double-click path is the same: `is_document` in `app/src-tauri/src/main.rs` admits `.sm` from the command line, and `request_open` hands it to the UI, where `openPath` in `App.tsx` calls `openImported`. The Open dialog (`pickOpenPath` in `api.ts`) lists `.sm` too.

## 4. The desktop shell (`app/src-tauri`)

| File | What lives there |
|---|---|
| `main.rs` | Tauri commands (thin wrappers over `quire-service`), the native file dialogs (`pick`), the native menu with its accelerators, Open Recent, crash recovery, files the system asks to open, the window setup. |
| `scope.rs` | The files and folders the app may touch: only those the user chose in a dialog, opened from the system, dropped on the window, or kept in the recent lists (saved in the app's data folder as `recent.json`). Every command that takes a path checks it here, so a new command that reads or writes a file must too; a new file dialog goes through `pick`, never the dialog plugin in the webview. |
| `print.rs` | Printing and PDF export through WebKit's print engine, at the exact paper size: AppKit's save job on macOS, GTK's "Print to File" backend on Linux (the printer is named in the user's language). Windows only prints, through the dialog. |
| `pdf.rs` | Finishing the exported PDF with `lopdf` (macOS and Linux): details, bookmarks, links, page labels, smaller photos. |
| `tauri.conf.json`, `capabilities/` | Window, bundle, updater and permission configuration. |
| `tests/` | The headless PDF export test (macOS and Linux; CI runs it on Linux, see [05-testing.md](05-testing.md)). |

## 5. The UI (`app/src`)

SolidJS with fine-grained signals and stores, plain CSS, no component library. Files group by what they do:

**State and plumbing**

| File | What lives there |
|---|---|
| `main.tsx` | Mounts `App`. |
| `App.tsx` | The window: layout, live evaluation, open/save/export, `runCommand` (every menu and palette command by id), keyboard shortcuts in a browser, startup. |
| `store.ts` | The editor state: document, report, selection, editing, viewport, undo history, layout of regions (`computeLayout`), region operations. |
| `api.ts` | The bridge to Rust: `call(cmd, args)` invokes a Tauri command, the WebAssembly engine (`isWeb`) or posts to `/api`. Also file dialogs (run by the desktop app through `api.pick`), menus, window close. |
| `engine.ts` | Loads `quire_web.wasm` and calls it, in the web version. |
| `latest.ts` | Live evaluation's requests: debounced, and only the newest answer is shown. |
| `types.ts` | TypeScript mirrors of the document JSON and of `quire-service`'s report. |
| `settings.ts`, `recent.ts` | App preferences and Open Recent, in `localStorage`. |
| `recovery.ts` | Crash recovery: a copy of unsaved changes, kept by the backend a few seconds after the last edit. |
| `notebook.ts` | Notebook state and logic (open, autosave, rename, move, search), path helpers. |

**The page**

| File | What lives there |
|---|---|
| `Canvas.tsx` | Scrolling, zoom, selection frames, dragging, the hidden input that receives typing. |
| `Sheet.tsx`, `pages.ts` | Paper sheets in the page view; page geometry, headers and footers. |
| `Region.tsx` | One region on the page; dispatches on `kind` (`math`, `text`, `image`, `area`, `table`, `toc`, `plot2d`). |
| `MathEditor.tsx` | Typing math: linear editing, Greek, LaTeX shortcuts, prose detection, ambiguity notes. |
| `richtext.ts` | Text regions: ProseMirror in the editor, Djot in the document (`toDjot`). Unchanged blocks are written back as their source; what the editor can't represent is kept as uneditable blocks. `richtext.cases.json` holds round-trip cases shared with the Rust tests in `text.rs`. |
| `TableEditor.tsx` | The spreadsheet grid. Evaluation is in `quire-eval/src/sheet`. |
| `Plot.tsx` | 2D plots as SVG; the engine samples, this file lays out, plays animation frames. |
| `plotGeometry.ts` | Plot geometry without the DOM: SMath marker sizes and shapes, equal scales, frame timing. |
| `math.ts` | AST → MathML. |
| `trace.ts`, `xref.ts`, `checks.ts`, `labels.ts` | Where a variable comes from; cross-references; pass/fail checks; caption words per language. |
| `nav.ts`, `find.ts` | Keyboard movement between regions; find and replace. |

**Chrome around the page**

| File | What lives there |
|---|---|
| `FormatBar.tsx` | The ribbon: Text, Paragraph, Equation, Arrange. |
| `Inspector.tsx` | Region, document and page properties. |
| `Sidebar.tsx` | The notebook sidebar: switcher, tree, search, the note title in the toolbar. |
| `Palette.tsx` | ⌘K: commands, variables, symbols and notes. |
| `Welcome.tsx`, `TemplatesDialog.tsx`, `SettingsDialog.tsx`, `HelpDialog.tsx`, `RefPicker.tsx`, `Updates.tsx` | Dialogs and banners. |
| `ContextMenu.tsx`, `menu.ts`, `Popover.tsx`, `Tooltip.tsx`, `Completion.tsx`, `FindBar.tsx` | Small floating UI. |
| `symbols.ts`, `insert.ts` | The symbol and structure catalogue behind the galleries and `\name` shortcuts. |

**Leaving and entering the app**

| File | What lives there |
|---|---|
| `actions.ts` | Clipboard actions, images, duplicate, context menus. |
| `clipboard.ts` | Clipboard formats: Quire regions as JSON, HTML/MathML and plain text for other apps, and turning pasted content into regions. |
| `latex.ts` | AST → LaTeX (Copy as LaTeX). |
| `csv.ts` | CSV in and out of tables. |
| `export.ts` | PDF details, image downsampling, link targets for `pdf.rs`. |
| `share.ts`, `ShareDialog.tsx` | Documents in links (`#doc=`, deflated and base64url) and the Share dialog. |

**Accessibility**

| File | What lives there |
|---|---|
| `a11y.ts` | `useModal` (focus traps), `arrowFocus`, F6 between panes, `announce` for the live region. |
| `speech.ts`, `describe.ts` | Math in words; a region in a sentence. |

**Styling.** Everything is in `styles.css`: design tokens on `:root` with dark values under `prefers-color-scheme: dark` (blocks add their own dark overrides where needed), then one block per part of the window (toolbar, canvas, regions, format bar, inspector, sidebar…), the accessibility block (high contrast) and the motion block at the end. Section headers (`/* ==== notebook sidebar */`) make it searchable.

## 6. The web version

`npm --prefix app run build:web` builds the same UI with `--mode web` (`app/.env.web` sets `VITE_QUIRE_WEB`), into `app/dist-web`, with the engine from `crates/quire-web` beside it. `isWeb` in `api.ts` switches what needs a file system: Open picks a file, Save downloads a `.quirez`, and notebooks, recent documents and your own templates are hidden. The document lives in the address bar (`keepLinkCurrent` in `App.tsx`), so a reload keeps it. In the desktop app and the browser setup, the same links open too: `#doc=` is read in every mode.

## 7. Everything else

| Path | What it is |
|---|---|
| `docs/` | This documentation; see [README.md](README.md). |
| `schema/quire-1.schema.json` | JSON Schema for `document.json`. |
| `tools/build-math-font.py` | Builds `app/src/fonts/QuireMath.woff` from STIX Two Math. |
| `app/e2e/` | Playwright tests. |
| `.github/workflows/ci.yml` | Tests on every push and pull request. |
| `.github/workflows/release.yml` | Installers and update packages for a `v*` tag. |
| `.github/workflows/pages.yml` | The web version on GitHub Pages, on every push to `main`. |
