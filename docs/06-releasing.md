# 06 — Releasing

How a version of Quire reaches users. Only maintainers release.

## 1. How it fits together

- Versions follow `0.MINOR.PATCH` for now: a patch for fixes and small features, a minor version for something people would notice in the notes' headline.
- One version number, in three places: `Cargo.toml` (`[workspace.package]`), `app/package.json` and `app/src-tauri/tauri.conf.json`.
- Pushing a `v*` tag runs `.github/workflows/release.yml`. Its first job checks that the tag is `v` + the version in all three files, that the commit is on `main`, and that CI passed on it (waiting up to an hour while CI is still running); any failure stops the release before anything is built. Then it builds on Windows, Linux (Ubuntu 22.04) and macOS (universal), signs the update packages, and attaches 14 files to a **draft** release, including `latest.json`. *Run workflow* in the Actions tab works from `main` only.
- The build job holds the updater key, so every action it uses is pinned by commit SHA; Dependabot (`.github/dependabot.yml`) proposes updates as pull requests. Review those like code.
- Rust is pinned in `rust-toolchain.toml`. Raise it in its own pull request, so a new compiler's warnings are fixed before a release depends on it.
- Installed copies ask `https://github.com/guidofraga/quire/releases/latest/download/latest.json` for updates (`tauri.conf.json` ▸ `plugins.updater`). That URL only follows **published** releases, so a draft is invisible to users until you publish it.
- Update packages are signed with the updater key; the app checks them against the public key in `tauri.conf.json` and refuses anything else.

The web version is separate: `.github/workflows/pages.yml` publishes it to https://guidofraga.github.io/quire/ each time CI passes on a push to `main` (from the commit CI checked), so it is usually a little ahead of the installers. A push whose CI fails is not published. Documents keep the same format, so links work in both.

## 2. Steps

1. **Main is green.** CI has passed on the commit you are releasing, and you have tried the app (`npm --prefix app run tauri dev`), including anything the tests can't reach: native dialogs, menus, printing and PDF (run the PDF test, see [05-testing.md](05-testing.md)).
2. **Bump the version** in the three files, then refresh the lockfiles:

   ```bash
   npm --prefix app install --package-lock-only
   ```

   ```bash
   cargo check
   ```

3. **Commit, tag and push:**

   ```bash
   git commit -am "Version 0.1.12"
   ```

   ```bash
   git tag v0.1.12
   ```

   ```bash
   git push origin main v0.1.12
   ```

4. **Wait for both workflows.** CI runs on the version commit; Release waits for it, then builds the draft (about 10–20 minutes more). If CI fails, Release stops without building: fix it on `main` and release a new patch version (never move the tag). If the version check fails, the three files disagree or the tag has a typo: delete the unpublished tag (`git push origin :v0.1.12`), fix, and tag again.
5. **Write the notes** (template below) and publish:

   ```bash
   gh release edit v0.1.12 --notes-file notes.md --draft=false --latest
   ```

6. **Check the updater sees it:**

   ```bash
   curl -sL https://github.com/guidofraga/quire/releases/latest/download/latest.json | head -5
   ```

   It should report the new version, with an entry for every platform.

## 3. Release notes

Written for users, not developers: what they can do now, in their words. Lead with the headline feature in one sentence.

```markdown
Quire 0.1.12 brings …: one sentence on what's new and why it matters.

If you're on 0.1.11, Quire offers this update by itself: choose **Update and Restart** on the banner.

## What's new
- **Feature.** What it does, how to reach it (menu path, shortcut).

## Improvements
- …

## Fixes
- What was wrong, as the user saw it.

## Downloads
| System | File |
|---|---|
| Windows | `Quire_0.1.12_x64-setup.exe` or `Quire_0.1.12_x64_en-US.msi` |
| macOS (Apple Silicon and Intel) | `Quire_0.1.12_universal.dmg` |
| Linux | `Quire_0.1.12_amd64.AppImage`, `.deb` or `.rpm` |

The other files (`.tar.gz`, `.sig`, `latest.json`) are for the in-app updater; you don't need to download them.

These builds are not code-signed yet, so Windows SmartScreen and macOS Gatekeeper warn on first launch:
- **Windows:** choose *More info ▸ Run anyway*.
- **macOS:** open Quire once and dismiss the message, then choose *System Settings ▸ Privacy & Security ▸ Open Anyway* and confirm.
```

Leave out sections that would be empty. Say if documents written by the new version need it to open (so far, none have).

## 4. The updater key

- The private key is `~/.tauri/quire.key` on the maintainer's machine, with its password in the Keychain, and in the repository secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
- **Never commit it. Back it up.** Without it, installed copies can never be updated again; a new key means every user reinstalls by hand.
- A local `tauri build` signs update packages and so needs the key. Without it, add `--config '{"bundle":{"createUpdaterArtifacts":false}}'`.

## 5. When something goes wrong

- **A platform failed to build:** re-run the failed job from the Actions tab; the draft collects the files of every job.
- **Release stopped at "CI passed on this commit":** CI failed or was still running after an hour. Once CI is green on that commit, re-run the Release workflow.
- **A bad release is out:** publish a fixed patch version quickly; the updater moves everyone to it. Deleting a published release makes `latest` fall back to the previous one, which is also an option for a release that can't be fixed fast.
- **Never move a published tag.** Make a new version instead.
