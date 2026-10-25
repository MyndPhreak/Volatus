# Volatus installer, releases and Explorer menu: design

Date: 2026-10-25. Status: draft for approval. Written after a conversation with the owner. Lines marked **(my call)** are the writer's defaults or guesses, not the owner's words. The model is the Negotium installer in `C:\Users\mynd\DEV\Negotium` (spec `docs/superpowers/specs/2026-10-17-negotium-installer-design.md`, WiX file `crates/app/wix/main.wxs`, workflows `.github/workflows/`), cut down to what Volatus needs.

## Intent

Other people, and the owner, should be able to download Volatus from GitHub, install it like a normal Windows program, start it from the Start menu, and right-click a folder in Explorer to scan it. Today Volatus only runs from `target\release`.

## What the owner decided

- The code lives in a public GitHub repository, **MyndPhreak/Volatus**. It is created and `main` is pushed (done, 2026-10-25).
- Distribution is a GitHub Release with an installer attached, the way Negotium does it.
- An Explorer context menu entry to scan a selected folder and the current folder is wanted. The owner suggested building the installer first, so the menu entries ship through the installer.
- Out for now, said earlier: auto-update, the Windows 11 short context menu, file operations in the menu, a headless print mode.

## Goals

1. **Release flow.** Like Negotium: `release-please` reads the commit messages (`feat:`, `fix:`) and keeps a release pull request with the version and a changelog. Merging it makes a draft release. A Windows runner builds the MSI, checks that the tag equals the `Cargo.toml` version, attaches the MSI, and publishes the release. Nobody builds installers by hand.
2. **CI on every push and pull request** on a Windows runner: clippy with `-D warnings`, the workspace tests. Tests marked `#[ignore]` (they need an elevated terminal and a real volume) do not run in CI. Slint tests run headless.
3. **MSI installer (WiX 3, as Negotium).** Per-machine, to `C:\Program Files\Volatus`, one UAC prompt at install. A Start menu shortcut. An entry in Windows "Installed apps" with the version and publisher "MyndPhreak". Upgrades replace the installed version in place; downgrades are refused. Uninstall removes the program, the shortcut and every registry entry the installer made.
4. **Explorer menu (a checkbox on the install dialog, ON by default (my call)).** Entries named "Scan with Volatus" for: a selected folder, the background of the current folder, and a drive (right-click in This PC). They start `volatus.exe` with the folder as its argument. They are registry entries written and removed by the installer, so no app flag is needed. They show in the classic menu ("Show more options" on Windows 11).
5. **Quote fix (a prerequisite).** Today the argument `"C:\"` reaches the app as `C:"` because the backslash escapes the quote. The menu runs `volatus.exe "%1"`, so right-clicking a drive would hit it. The argument parser must repair a path that ends in a stray quote (for example `C:"` becomes `C:\`). This is test-first and ships with the installer work.
6. **Running copy.** Installing or upgrading while Volatus runs: the installer asks the running copy to exit, or tells the user to close it, before files are replaced.

## Non-goals and anti-goals

- No code signing yet. SmartScreen will warn on first download ("More info, Run anyway"). Signing is a later item.
- No auto-update, winget or Microsoft Store entry, portable zip, beta channel.
- No Windows 11 short-menu entry (needs a COM handler and a signed package).
- Anti-goals: an install that leaves the exe in a folder an ordinary user can write to (the elevated "Restart as administrator" would then run a file anyone could swap); a menu entry that points at a removed exe after uninstall; any registry write outside the installer's own keys; a test that writes to the real Explorer keys.

## Constraints

- Rust workspace with `app`, `scan`, `treemap`; the binary is `volatus.exe`; version `0.1.0` today in `Cargo.toml`. Icon: `app/assets/volatus.ico` (already embedded in the exe).
- WiX Toolset v3 as in Negotium (pinned download with a SHA-256 check in the workflow). Workflows follow Negotium's permissions style (each job asks only for what it needs).
- Menu command line must work with paths that contain spaces, unicode and (for drives) a trailing backslash.
- Engineering rules: plan first, tests first for pure logic, deviations recorded. The WiX file is checked by tests like Negotium's `wix.rs` (builder decides how far to go). Nothing is published (no release created, no tag) without the owner's go-ahead at that step.
- UI and installer text in plain ASD-STE100 style.

## My calls (the owner has not seen these as decisions)

- **Per-machine to Program Files, not per-user.** Reason: the "Restart as administrator" button launches the same exe elevated. In a per-user folder any program running as the user could replace it first. This changes the guess made earlier in the conversation.
- Menu checkbox ON by default; no PATH option (YAGNI); no "launch Volatus" checkbox needed but the builder may add one on the last page as Negotium does.
- The Explorer entries are for every user of the machine (they live under `HKLM\Software\Classes`). The label is "Scan with Volatus" and the icon is the exe's.
- The installer does not run `volatus.exe` to do its work (Negotium's `-integrate` flag is not copied). Registry entries are plain WiX elements, so there is no custom action to fail.
- The release job only runs after the release-please pull request is merged; the first release is `v0.1.0` (version already in `Cargo.toml`) or whatever release-please computes from the commit history (builder checks how to seed it, since the history has no earlier tag).

## Left to the builder

- The `.wxs` layout, upgrade code (a new GUID, fixed forever), feature and dialog flow, the cargo-wix or direct WiX invocation, and the version flow from `Cargo.toml`.
- The workflow files and the `release-please` configuration (config and manifest), cache settings, runner image pinning.
- How to test the WiX file and the generated registry values without writing to the real machine (parse the `.wxs`, build the MSI in CI, optionally install to a throwaway path in a VM or with `msiexec /a` administrative image).
- The README install section and `docs/USAGE.md` update.

## More calls, after a cold read (all mine; mark them as such)

- **Running copy.** No app flag. The MSI uses WiX `util:CloseApplication` (a graceful close of the main window; it also reaches an elevated copy because the MSI runs elevated) and, if that fails, Windows' own "files in use" dialog asks the user to close Volatus. No forced kill, and no reboot unless a file stays locked in another user's session.
- **Instances.** Each right-click on a folder opens a new window. No single-instance rule and no IPC. That is a later feature if wanted.
- **Versions and commits.** Same flow as Negotium: work happens on branches with the project's normal commit messages ("Add failing tests for X", "Implement X"). Changes reach `main` through pull requests that are squash-merged with a conventional title (`feat: ...`, `fix: ...`), which `release-please` reads. The first release is forced to `0.1.0` (`release-as` plus a bootstrap commit set to the current `main`), so the 151 earlier commits do not produce a surprise version. The version source is `app/Cargo.toml`; the tag check and WiX read the `volatus` package from `cargo metadata`.
- **First release.** The plan stops at "workflows merged and the release pull request open". Merging that pull request is the owner's go-ahead for the first real release. CI also builds the MSI on every pull request, so it is tested before any tag. Like Negotium, release-please uses the default `GITHUB_TOKEN`, so a release pull request does not start other workflows by itself (close and reopen it to run CI). No secret is needed.
- **Quote fix, two layers.** (1) The menu command avoids the problem: it passes `"%1."` for folders and drives, so no backslash sits before the closing quote (Windows drops a trailing dot, and `C:\.` names the drive root). (2) The argument parser also repairs the common case, a folder argument that ends in a stray quote (`C:"` becomes `C:\`, `C:\Some Dir"` becomes `C:\Some Dir\`), only for the folder argument and only when the repaired path exists. Other malformed arguments stay as they are. Test matrix: drive root, folder with spaces, unicode, trailing backslash, quote in the middle (unchanged), lone quote (unchanged), a flag after the folder.
- **Installer UI.** Copy Negotium's flow: minimal WiX UI, a license page showing the MIT text, the Explorer-menu checkbox, a "Launch Volatus" checkbox on the last page. English (en-US) only.
- **Target.** x64 only (the build is `x86_64-pc-windows-msvc`). A launch condition requires 64-bit Windows 10 or newer. The app is tested only on Windows 11, but Windows 10 is not known to break (Mica falls back to an opaque background). ARM64 is refused.

## Open items for the owner

One: the per-machine install to Program Files (one UAC prompt at install) is the writer's call, not yet confirmed by the owner.
