# 04 — Guidelines

How Quire is written, and how to make common kinds of change. [CONTRIBUTING.md](../CONTRIBUTING.md) covers setup and the pull-request checklist; [03-code-map.md](03-code-map.md) says where things are.

## 1. What we are building

Quire is a calculation notebook: you write maths with units the way you would on paper, and it works out the results as you type. It is for engineers writing calculation reports, and for students and teachers in STEM keeping course notes and lab reports. These principles settle most design questions:

- **Results are trustworthy.** A result never changes by accident, units are checked everywhere, and an error says what is wrong in the user's terms. Correctness beats features.
- **Nothing is lost.** Notes save themselves, unsaved documents are recovered after a crash, deletes go to the system Trash, and every change can be undone.
- **Files belong to the user.** Documents are readable, diffable JSON; a notebook is an ordinary folder. Anything Quire writes must still open in later versions, and unknown keys survive a round trip.
- **It feels like the best desktop software.** Smooth, quiet, keyboard-friendly, at home on macOS, Windows and Linux. Polish is part of the feature, not a later step.
- **Familiar to SMath and Mathcad users.** Where they have a convention (`:=`, `=`, `→`, `rkfixed`, units after a number), we follow it unless it is clearly worse.
- **Everyone can use it.** Keyboard alone, with a screen reader, with Reduce motion or Increase contrast.

## 2. Writing code

### Everywhere

- Match the code around you: naming, comment density, file layout. Read the neighbouring functions before writing a new one.
- Comments explain *why* (a constraint, a trade-off, a bug a line works around), not what the next line does. Module headers say what the file is for, in a sentence or two.
- Keep functions small and named for what they do. Prefer a plain loop to a clever abstraction.
- Don't reformat code you aren't otherwise changing.

### Rust

- `rustfmt` with `rustfmt.toml` (110 columns), and `clippy -D warnings`.
- Edition 2024, Rust 1.90 or newer (`Cargo.toml` `rust-version`).
- Engine errors are values (`quire_eval::Error`, `ServiceError`), never panics: a document is untrusted input, and any expression a user can type must give an error message, not a crash.
- Public items get a `///` comment when the name doesn't say everything. Each module starts with `//!`.
- Tests live next to the code in `#[cfg(test)] mod tests` (or a sibling `tests.rs` when they are long, as in `symbolic/`).

### TypeScript and SolidJS

- TypeScript strict mode, formatted by Prettier (`app/.prettierrc.json`: 120 columns); `npm run format` formats, and `npm run format:check` and `npx tsc --noEmit` must pass.
- Components are functions in `PascalCase.tsx`; logic without UI lives in `camelCase.ts` so it can be tested with Vitest.
- **Change the document only through `mutate()`** (`store.ts`): it records the undo step and schedules evaluation. `mutate(fn, false)` is for changes that join the previous step. Never write to `state.doc` with `setState` directly.
- Read component props through `props.x` (destructuring breaks reactivity). Derived values that many components read go in a `createMemo` or `createRoot` at module level, as `layout` does.
- Pass `unwrap(state.doc)` to the backend, not the store proxy.
- Backend calls go through `api.ts`; components never call `invoke` or `fetch` themselves.
- Errors a user should see go through `flash()` (a toast that is also announced); never `alert()`.

### CSS

- One stylesheet, `app/src/styles.css`, in blocks with searchable headers. Add a block for a new part of the window, or extend the block it belongs to.
- **Colours come from tokens** (`--ink`, `--muted`, `--accent`, `--chrome`, `--sidebar`…) defined on `:root`. A new colour needs a dark value under `@media (prefers-color-scheme: dark)` and, if it carries meaning, a high-contrast one in the accessibility block.
- Layout that depends on the space available uses the `main` container (`@container main (max-width: …)`), not the window width, because the sidebar takes part of the window.
- **Motion** is short (120–240 ms) and eased with `var(--ease-out)`. It shows where something comes from (the sidebar slides, a section unfolds) and never delays work. The `prefers-reduced-motion` rule in the accessibility block turns it off; animations made in script check the same preference (`fadeInPage` in `notebook.ts`).
- Positions on the page snap to the 9 px grid (`GRID` in `store.ts`). In the chrome, match the sizes of the controls next to yours rather than inventing new ones.

## 3. Words

What the user reads is part of the design.

- **Plain, specific and short.** "cannot add a length (m) and a force (N)", not "dimension mismatch". "x is defined further down", not "undefined symbol".
- Say what to do next when there is something to do: "did you mean the unit kN?".
- Menus and buttons: Title Case for menu items (*Export as PDF…*), sentence case for buttons and labels (*New section*). An ellipsis means the item asks for more before acting.
- Shortcuts in documentation use the Mac symbols (⌘ ⌥ ⇧); the app shows Ctrl on Windows and Linux by itself.
- British or American spelling: follow the file you are in; user-facing text says *colour* and *metres* today.
- No exclamation marks, no blame ("you entered an invalid…"), no jargon a student wouldn't know.

## 4. Accessibility

Quire has to work with the keyboard alone and with a screen reader. `app/e2e/a11y.spec.ts` runs [axe](https://github.com/dequelabs/axe-core) on the main states of the app in light and dark and fails on any serious or critical violation; it also walks the keyboard flows. When you add UI:

- **Use real controls.** A `<button>` for anything clickable; an icon-only button gets an `aria-label`, and decorative glyphs get `aria-hidden="true"`. Toggles say `aria-pressed`; things that open say `aria-expanded` and `aria-haspopup`.
- **Dialogs** have `role="dialog"`, `aria-modal="true"` and a name, and call `useModal` (`a11y.ts`): it moves focus in, keeps Tab inside and gives focus back on close. Esc closes.
- **Menus, galleries and trees** move with the arrow keys (`arrowFocus`, or the tree's own `treeKeys` in `Sidebar.tsx`) and take focus only when opened from the keyboard, so the text being edited keeps its caret when the mouse is used.
- **F6** cycles between the parts of the window (`PANES` in `a11y.ts`); a new pane joins that list.
- **Math** is hidden from screen readers as MathML and read as words (`speech.ts`); a new expression kind needs words there too, with a test.
- **Messages** a person should hear without looking go through `flash` or `announce`.
- **Colours** reach 4.5:1 for text in both themes. Hidden content is `inert` or `aria-hidden`, not merely transparent.
- Add the new state to `a11y.spec.ts` (or the feature's own spec) so axe sees it.

## 5. Documents and compatibility

- The format is specified in [02-file-format.md](02-file-format.md) and `schema/quire-1.schema.json`. A change to what is written updates both in the same commit.
- **Old documents keep opening unchanged.** Add fields; don't rename or reinterpret existing ones. When an old spelling must change, read both (see the unquoted Djot colours in `text.rs`).
- **Unknown keys survive.** `quire-doc` keeps them in `extra` maps and the app holds the document as JSON, so a newer file edited by an older Quire loses nothing.
- **Canonical output.** `canonical.rs` fixes the key order and line breaks so diffs stay small; the SMath importer writes through it too.
- No timestamps, counters or machine-specific paths in a document.
- Documents are untrusted: text HTML is sanitised, only `http(s)`, `mailto` and in-document links survive, and nothing in a document runs code.

## 6. Performance

- A whole document re-evaluates on every change; keep a typical document well under a millisecond (`quire eval` prints the time). An engine change that makes evaluation slower needs a reason.
- In the UI, prefer signals and memos to re-rendering lists; `Region.tsx` re-renders one region, not the page.
- Work that can wait (autosave, search, update checks) is debounced or deferred and never blocks typing.

## 7. Dependencies and licensing

- Add a dependency only when it earns its place: something hard to get right (ZIP, PDF objects, ProseMirror), not something a short function does.
- **Engine crates** (`crates/`) may only use code compatible with MIT OR Apache-2.0; never GPL. **The app** (`app/`) is GPL-3.0-or-later and may use GPL-compatible code.
- Don't copy code, unit tables or documents from other programs (SMath Studio and Mathcad included) unless their license allows it. Factors come from primary sources (NIST SP 811, CODATA).

## 8. Recipes

### Add a built-in function

1. Implement it in `quire-eval`: in `builtins.rs` `call` when its arguments are plain values, in `stats.rs` or `solvers.rs` when it belongs with those, or in `interp.rs` when it must see its arguments unevaluated (like `int` or `sum`). Use `arity` and `vector` for argument checks so the errors read like the others.
2. Test it next to the code, with units: a slope of kN against m is in kN/m.
3. If `→` should handle it, teach `symbolic/` (and add a case to `symbolic/tests.rs`).
4. Offer it in the UI: an entry in `symbols.ts` (galleries, ⌘K and `\name` completion) and, if it is common, a line in `HelpDialog.tsx`.
5. Only if it should be read or typeset differently from `name(args)`: `speech.ts`, `math.ts`, `latex.ts`.

Spreadsheet formulas reach every worksheet function automatically (`sheet/functions.rs` falls through to them).

### Add a unit or constant

Units go in the table in `quire-units/src/db.rs`, constants in `quire-eval/src/constants.rs`, each with a test of its factor and source. The galleries list them through `quire_service::library()` with no UI change. Check that a new name doesn't shadow a common variable name; the ambiguity notes in `MathEditor.tsx` show how `g` is handled.

### Add a command

1. Handle its id in `runCommand` in `App.tsx`.
2. Add it to the native menu in `app/src-tauri/src/main.rs` (`menu()`), with an accelerator if it has one.
3. Add it to `COMMANDS` in `Palette.tsx` so ⌘K finds it.
4. If it has a shortcut, map it in `browserShortcuts` in `App.tsx` too, so the browser setup and the e2e tests can use it.

### Add a backend call

1. A function in `quire-service` that takes and returns JSON-friendly types.
2. A `#[tauri::command]` wrapper in `main.rs`, listed in `generate_handler!`. If it takes a path, check it with `allowed` first (`scope.rs`): the app only touches files the user chose.
3. The same name in the `dispatch` match in `crates/quire-cli/src/serve.rs`, and in `crates/quire-web/src/lib.rs` if the browser can do it (no files: data comes and goes as JSON or base64).
4. An entry in `api` in `app/src/api.ts`.

Notebook operations skip steps 2 and 3: add an `op` to `notebook::call` and both transports pick it up. Its paths are relative to the notebook (`inside`); the app checks only the folder `notebook::folder` names.

### Add a region kind

1. Specify it in [02-file-format.md §4](02-file-format.md) and the schema.
2. Evaluate or render it in `quire-service` if the engine is involved (`math_inputs`, `report`), and mirror the view in `types.ts`.
3. A `<Match>` in `Region.tsx`, and an insert command (recipe above).
4. Make it travel: `clipboard.ts` (between documents and to other apps), `describe.ts` (screen readers), print and PDF (check the page view), find if it holds text.
5. Tests: Rust for the engine part, Vitest for logic, one e2e step that inserts it.

### Change the file format

Update [02-file-format.md](02-file-format.md), the schema, `quire-doc`, `canonical.rs` (key order) and the SMath importer (`crates/quire-smath`) together, add a fixture that an older version wrote, and test that it still opens and evaluates the same.
