# 05 — Testing

What is tested where, how to run it, and what a good test looks like. CI (`.github/workflows/ci.yml`) runs every layer below on every push and pull request: the PDF export test on Linux, and the desktop app's lint and unit tests on Linux, macOS and Windows. The SMath corpus is not in the repository, so there the corpus tests check only the fixtures that are.

## 1. The layers

| Layer | Where | Run | Tests what |
|---|---|---|---|
| Engine unit tests | `#[cfg(test)] mod tests` next to the code; `quire-eval/src/symbolic/tests.rs` | `cargo test --workspace --exclude quire-app` | Parsing, units, evaluation, tables, symbolic math, notebooks, file I/O |
| Worksheet fixtures | `crates/quire-cli/tests/fixtures/*.quire` with `results.json`, checked by `tests/check.rs` | the same `cargo test` | Whole documents against results worked out independently |
| UI logic | `app/src/*.test.ts` (Vitest, jsdom) | `npm --prefix app test` (`run test:watch` while you work) | Store and undo, clipboard, CSV, LaTeX, speech, pages, notebook paths |
| End to end | `app/e2e/*.spec.ts` (Playwright, WebKit) | `npm --prefix app run e2e` | The UI against `quire serve`, typed as a user would; `web.spec.ts` runs the web version with its WebAssembly engine (project `web`) |
| SMath examples | `crates/quire-cli/tests/smath_examples.rs` | `cargo test -p quire-cli --test smath_examples -- --nocapture` | Every worksheet in the SMath corpus imports, evaluates, and reproduces at least its floor of SMath's cached results |
| SMath robustness | `crates/quire-cli/tests/smath_robustness.rs` | the same `cargo test` | The importer never panics on hostile input, corpus prefixes or mutations |
| Accessibility | `app/e2e/a11y.spec.ts`, plus axe in `notebook.spec.ts` | part of `e2e` | axe in light and dark; keyboard flows |
| Desktop app | `#[cfg(test)]` in `app/src-tauri/src` (`scope.rs`, `pdf.rs`) | `npm --prefix app run build`, then `cargo test -p quire-app` | Which paths the app may touch; finishing exported PDFs |
| PDF export | `app/src-tauri/tests/` (macOS and Linux; CI runs it on Linux under `xvfb-run`) | `npm --prefix app run build`, then `cargo test -p quire-app --features tauri/custom-protocol -- --include-ignored` | Exports `fixtures/report.quire` headlessly and checks the PDF's details, bookmarks and links; also shows the app and its menu start |

CI runs the PDF test on Linux only. When you change printing, `export.ts`, `print.rs` or `pdf.rs`, run it on a Mac too: macOS writes the PDF through AppKit, which CI doesn't cover.

## 2. The README's pictures

`npm --prefix app run screenshots` (`app/screenshots/`) opens the documents in `docs/examples/` and the built-in templates in the real app, in WebKit against `quire serve`, and writes `docs/images/`. It isn't part of `npm run e2e`. Run it after a visible change to the app and commit the pictures that changed.

## 3. Writing tests

- **Say what a user typed and what they should see.** `M := q*L^2/8 =` with `q := 12 kN/m` and `L := 6 m` shows `54 kN·m`: that test documents behaviour and catches regressions in parsing, units and display at once.
- **Test with units.** Most engine bugs hide in dimensions: a fit of kN against m, an ODE with t in seconds.
- **For symbolic results, check by numbers too.** `symbolic/tests.rs` puts values into both sides, and differentiates what `int` gives, so a correct but differently written answer still passes.
- **One behaviour per test**, named as a sentence: `moves_between_sections_but_not_into_themselves`, `"search finds a note by its content, ignoring accents"`.
- **A bug fix starts with a failing test.**
- Tests that touch the disk work in a fresh temporary folder and check what lands there, not only what the screen shows (`notebook.spec.ts`).

## 4. End-to-end tests

`npm run e2e` starts `quire serve` and the Vite dev server (or reuses ones already running), then drives WebKit, the engine of the macOS app. Once per machine: `npx --prefix app playwright install webkit`.

- `QUIRE_UI_PORT` and `QUIRE_API_PORT` move both servers, so several checkouts can run side by side. The web version runs on `QUIRE_WEB_PORT` (the UI port + 1), and needs the WebAssembly target once: `rustup target add wasm32-unknown-unknown`.
- `--project web` runs only the web version's tests; `--project webkit` only the desktop UI's.
- `npx playwright test e2e/notebook.spec.ts -g "keyboard"` runs one test; `--headed` or `--debug` shows the browser.
- On failure, traces are kept in `app/test-results/` (CI uploads them as `playwright-traces`); open one with `npx playwright show-trace <zip>`.
- Find elements the way a user would: by role and name (`getByRole("button", { name: "New section" })`), not by CSS class. A selector that can't be written by role is often an accessibility bug.
- Wait for state, never for time: `await expect(...).toBeVisible()`, `expect.poll(() => existsSync(...))`. A fixed `waitForTimeout` is a last resort with a comment saying why.
- The test browser reports itself as a Mac, so shortcuts are written with `ControlOrMeta` or `Meta`, wherever the tests run (CI is Linux).

### Flaky tests

A test that fails sometimes is a bug, in the test or in the app. Reproduce it with `--repeat-each 20` before changing anything. Known causes so far:

- typing before a field has focus (wait for `toBeFocused()` first, as the `name()` helper in `notebook.spec.ts` does);
- measuring colours mid-animation (the axe tests emulate `reducedMotion: "reduce"`);
- CI's Linux WebKit reporting text colours mid-update right after a document opens (the `audit()` helper in `e2e/axe.ts` re-measures before failing, so only problems that persist fail).

## 5. Results that must not change

`crates/quire-cli/tests/check.rs` evaluates each bundle in `tests/fixtures` and compares every result with its `results.json`, whose values were worked out by hand or in another program. When a change alters what an existing worksheet shows, the test fails; if the new result is right, say so and why in the commit, and update `results.json`.

Documents you can't share (client reports, SMath files) can be checked locally:

```bash
QUIRE_CORPUS="../private/reports:../private/one.quire" cargo test -p quire-cli --test check
```

`QUIRE_CORPUS` takes `.quire` bundles or folders of them, separated by `:`, each with a `results.json`. `quire check <doc>` compares a single document and prints the differences. An SMath file imported with `quire import` carries SMath's own cached results, so it can be checked the same way (`QUIRE_SMATH_REFERENCE`, see `tests/reference_results.rs`).

### The SMath corpus

`crates/quire-cli/tests/smath_examples.rs` imports every `.sm` file in the corpus and checks its results against the ones SMath cached, the way `quire check` does. The corpus is not in the repository: set `QUIRE_SMATH_EXAMPLES` to a folder of `.sm` files, or put them in `examples/` (without a corpus the test prints a note and passes).

The test is a ratchet. `FLOOR` in the file holds, for each worksheet, the number of results that must match. A change may raise a count but must not lower it; when a fix lands, raise its floor in the same change. Run it with `--nocapture` to see the table of matched and total results per file. Every file needs a floor, and importing or evaluating any of them must not fail or panic.

`smath_robustness.rs` feeds the importer broken input: hostile fixtures, every corpus file cut at many positions, and random mutations. Set `QUIRE_SMATH_PANIC_DIR` to save each input that panics, for reproduction.

## 6. Before you push

```bash
cargo fmt --all --check
cargo clippy --workspace --exclude quire-app --all-targets -- -D warnings
cargo test --workspace --exclude quire-app
cd app && npm run format:check && npx tsc --noEmit && npm test && npm run e2e
```

If you changed `app/src-tauri`, also (after `npm --prefix app run build`):

```bash
cargo clippy -p quire-app --all-targets -- -D warnings
cargo test -p quire-app
```

And try UI changes by hand, in the browser setup or `npm --prefix app run tauri dev`, in light and dark and in a narrow window.
