# Contributing to Quire

Thanks for helping. This page gets you set up and says what a good change looks like. The details live in [docs/](docs/README.md):

- [Code map](docs/03-code-map.md): where everything is, and how an edit becomes a result on the page.
- [Guidelines](docs/04-guidelines.md): how code, styles and the words in the app are written; accessibility; recipes for common changes.
- [Testing](docs/05-testing.md): what is tested where, and how to write a test that lasts.
- [Releasing](docs/06-releasing.md): how a version reaches users (maintainers).

If something here is unclear or out of date, fixing it is a welcome first contribution.

## Where things are

| Path | What it is |
|---|---|
| `crates/quire-ast` | Linear math syntax: lexer, parser, canonical printer |
| `crates/quire-units` | Dimensions and the unit table |
| `crates/quire-eval` | The engine: evaluation, worksheets, spreadsheet tables, plots, symbolic math |
| `crates/quire-doc` | Loading `.quire` and `.quirez` documents |
| `crates/quire-service` | What the app asks the engine for; templates; notebooks |
| `crates/quire-cli` | The `quire` command, and `quire serve` for UI work in a browser |
| `app/` | The desktop app: SolidJS front end in `app/src`, Tauri shell in `app/src-tauri`, Playwright tests in `app/e2e` |
| `docs/` | Architecture, file format, code map, guidelines, testing, releasing |
| `schema/` | JSON Schema for `document.json` |
| `tools/` | The SMath importer and the math-font build script |

## Getting set up

You need [rustup](https://rustup.rs) and Node 22. `rust-toolchain.toml` picks the Rust version, with clippy, rustfmt and the WebAssembly target (for the web version); rustup installs it the first time you run `cargo` here. The code also builds with Rust 1.90 (`rust-version` in `Cargo.toml`), which CI checks. For the desktop app, also the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your system.

```bash
npm --prefix app ci
```

Day to day:

| To | Run |
|---|---|
| Run the desktop app while developing | `npm --prefix app run tauri dev` |
| Work on the UI in a browser | `cargo run -p quire-cli -- serve` in one terminal, `npm --prefix app run dev` in another, then open http://localhost:5173 (add `?open=<path to a .quire>` or `?notebook=<folder>`) |
| Work on the web version | `npm --prefix app run dev:web` (builds the WebAssembly engine, then serves the UI on its own) |
| Build the web version | `npm --prefix app run build:web` (into `app/dist-web`) |
| Test the engine | `cargo test --workspace --exclude quire-app` |
| Test the UI's logic | `npm --prefix app test` (Vitest; `run test:watch` while you work) |
| Try the UI end to end | `npm --prefix app run e2e` (Playwright in WebKit; once: `npx --prefix app playwright install webkit`) |
| Retake the README's pictures | `npm --prefix app run screenshots` (writes `docs/images`) |
| Test the desktop app, and check PDF export (macOS, Linux) | `npm --prefix app run build`, then `cargo test -p quire-app --features tauri/custom-protocol -- --include-ignored` |
| Build `Quire.app` / the installers | `npm --prefix app run tauri build` (add `-- --bundles app` for just the macOS app) |
| Try one expression | `cargo run -q -p quire-cli -- calc "2 m + 30 cm" --in mm` |

A local release build signs update packages and so needs the updater's private key. Without it, add `--config '{"bundle":{"createUpdaterArtifacts":false}}'` to the build command.

## Before you open a pull request

CI runs these on every push; run them first:

```bash
cargo fmt --all --check
cargo clippy --workspace --exclude quire-app --all-targets -- -D warnings
cargo test --workspace --exclude quire-app
cd app && npm run format:check && npx tsc --noEmit && npm test && npm run e2e
```

If you changed the desktop shell (`app/src-tauri`), also run `cargo clippy -p quire-app --all-targets -- -D warnings` and `cargo test -p quire-app` after `npm --prefix app run build`. CI runs them on Linux, macOS and Windows, and builds the web version (`npm --prefix app run build:web`).

And:

- **Changes come with tests.** Engine tests next to the code, UI logic in a `*.test.ts` next to the module, what a user sees end to end in `app/e2e`. See [Testing](docs/05-testing.md).
- **UI changes are also tried by hand**, in the browser setup or the desktop app, in light and dark. Say in the pull request what you tried, and add a screenshot when something looks different.
- **New UI works from the keyboard and with a screen reader**, and passes the axe tests. See [Guidelines §4](docs/04-guidelines.md#4-accessibility).
- **File-format changes** update `docs/02-file-format.md` and `schema/quire-1.schema.json` in the same change. Documents written by earlier versions must keep opening unchanged.
- **Results must not change by accident.** If a change alters what an existing worksheet shows, say so and why.
- **The desktop app only touches files the user chose.** A command that takes a path checks it against the scope (`app/src-tauri/src/scope.rs`), and file dialogs run in Rust (`pick`), not in the webview.
- **Workflows pin actions by commit**, with the version in a comment (`uses: actions/checkout@<sha> # v7.0.1`). Dependabot keeps them current.
- **Docs stay true.** If you move code or change a convention, update the page in `docs/` that describes it.

## Commits

- One change per commit: a fix, a feature or a refactor, not all three.
- The subject line says what the commit does, as a command, in 72 characters or fewer, with no full stop: `Print spreadsheet tables in PDFs`.
- Add a body when the reason isn't obvious from the diff: what was wrong, why this fixes it, what else it affects.
- Don't add generated footers or tool trailers.

## Branches and pull requests

- Branch from `main` with a short descriptive name (`fix/table-print`, `plot-zoom`) and keep the branch focused on one thing.
- Rebase on `main` rather than merging `main` into your branch.
- The pull request says what changed, why, and how you tested it. Link the issue it closes.
- CI must be green before merging. Changes to `main` go through pull requests.

## Reporting bugs and ideas

Open an issue with:

- what you did, what you expected and what happened instead;
- the Quire version (*Quire ▸ About Quire*) and your system;
- if a document is involved, the smallest `.quire` file that shows the problem.

**Don't attach confidential documents** (client calculations, reports) to issues or pull requests: they are public. Cut the document down to the part that shows the problem, with made-up numbers.

## Licensing of contributions

- The engine crates, the file-format spec and the schema are **MIT OR Apache-2.0**.
- The desktop app (`app/`) is **GPL-3.0-or-later**.

By contributing, you agree that your contribution is licensed under the terms of the part of the project it changes, and confirm that you have the right to contribute it. Don't copy code, unit tables or documents from other programs (SMath Studio included) unless their license allows it.

## Be kind

Assume good intent, keep discussions about the work, and help newcomers find their way. Harassment or personal attacks aren't welcome here.
