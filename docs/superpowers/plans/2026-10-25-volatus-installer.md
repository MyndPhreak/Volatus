plan-version: 1

# Volatus installer, releases and Explorer menu: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Volatus installs from a GitHub Release MSI (per-machine, Program Files), starts from the Start menu, and offers "Scan with Volatus" on folders, folder backgrounds and drives. Releases are cut by release-please and built by CI.
**Architecture:** A pure argument repair in `app/src/cli.rs`; a WiX 3 source `app/wix/main.wxs` built by cargo-wix 0.3.9; plain Registry elements for the menu (no custom action runs Volatus); one shared PowerShell build script used by CI and the release workflow; static checks on the `.wxs` plus a table check on the built MSI.
**Tech Stack:** Rust 2024, WiX Toolset 3.14 (`wix3141rtm`), cargo-wix 0.3.9, roxmltree 0.21.1 (already in `Cargo.lock:3841`), GitHub Actions, release-please-action v5.
**Spec:** `docs/superpowers/specs/2026-10-25-volatus-installer-design.md`. Model: Negotium (`C:\Users\mynd\DEV\Negotium`, read-only).

## Global Constraints
- Agents never push, tag, open pull requests, change repository settings or publish. Steps marked **OWNER GATE** are done by the chief after the owner says yes at that moment.
- Package name is `app` (`app/Cargo.toml:2`), bin `volatus` (`app/Cargo.toml:9-11`). The spec says "the `volatus` package": wrong. Scripts select the package whose bin target is `volatus`. No rename.
- Version source: `app/Cargo.toml` `package.version` (0.1.0). MSI versions must be `X.Y.Z`, X,Y <= 255, Z <= 65535.
- Install dir `C:\Program Files\Volatus`; publisher "MyndPhreak"; product name "Volatus"; Start menu shortcut "Volatus"; menu label "Scan with Volatus". English (en-US) only; all installer text ASD-STE100.
- Registry writes: only `HKLM\Software\Classes\{Directory\shell, Directory\Background\shell, Drive\shell}\Volatus` (and their `command` subkeys). No HKCU, no HKCR root, nothing else. MSI-owned ARP data is not in the Registry table.
- Tests never write the registry, never install the MSI, never start `volatus.exe`. Elevated tests stay `#[ignore]`.
- Baseline before P1: run `cargo test --workspace` and record the counts in the P1 report (ledger says 726 at P10 fix1). Every phase ends with 0 failed and `cargo clippy --workspace --all-targets -- -D warnings` clean.
- WiX pin (reuse Negotium's, proven by its 0.10.0 release): tag `wix3141rtm`, `wix314-binaries.zip`, SHA-256 `6AC824E1642D6F7277D0ED7EA09411A508F6116BA6FAE0AA5F2C7DAA2FF43D31`. Locally the builder downloads it to the scratchpad, never into the repo.

## Review Focus (inputs the tests could miss)
1. **Paths from Explorer**: drive root (`C:\` gives argv `C:"`), folder with spaces, unicode, `\\server\share`, background of a drive root (`%V` = `C:\`), user profile with spaces. Pinned by P1.T1 matrix and P3.T3 `menu_command_lines_round_trip_through_the_parser`; manual M5-M7.
2. **Upgrade while Volatus runs (normal and elevated)**: CloseApplication must run before InstallValidate and RemoveExistingProducts; elevated copies are not reachable by WM_CLOSE (UIPI), so the Files in Use dialog is the fallback; never a reboot prompt. Pinned by P3.T4 wix tests + `check-msi.ps1` sequence check on the built MSI; manual M9, M10.
3. **Uninstall leaves no menu entry**: ForceDeleteOnUninstall only on the three `...\shell\Volatus` keys, never on `...\shell`. Pinned by P3.T3 `only_the_volatus_verb_keys_are_force_deleted`; manual M12.
4. **Same version twice, older version, the same MSI twice**: same-version rebuild replaces in place (AllowSameVersionUpgrades), downgrade refused with a message, the same file opens Repair/Remove. Pinned by P3.T2 `it_installs_per_machine_and_refuses_downgrades`; manual M11, M13.
5. **UAC cancelled by a standard user / installer run twice at once**: nothing installed, no partial keys (Windows Installer transaction; menu is not a custom action). Exempt from automated tests (needs a real install); manual M3, M14.
6. **Menu choice kept across upgrades**: feature `ExplorerMenu` migrates its state (MajorUpgrade `MigrateFeatures` default yes); the options page shows on first install only. Pinned by P3.T3 `the_menu_is_a_feature_on_by_default_offered_on_first_install_only`; manual M8.
7. **Machine without the VC++ Redistributable**: static CRT, imports checked on every MSI build. Pinned by P3.T1 `check-imports.ps1`; manual M2 (Windows Sandbox).

---

## Summary
Today Volatus runs only from `target\release`. This work adds: (P1) a repair in the argument parser so a folder argument ending in a stray quote (`C:"`, from `"C:\"`) becomes `C:\`; (P2) CI on a Windows runner; (P3) a WiX 3 MSI with a Start menu shortcut, ARP entry, major upgrades, a launch condition, a graceful close of a running copy, a license page, an "Explorer menu" checkbox (on by default) that installs three HKLM registry verbs, and a "Launch Volatus" checkbox; (P4) release-please plus a release workflow that builds, checks and attaches the MSI and then publishes the draft; (P5) docs; (P6) owner steps up to "release pull request open".

## Architectural Decisions
| Decision | Reason |
|---|---|
| Menu commands `"[INSTALLFOLDER]volatus.exe" "%1"` (Directory, Drive) and `"[INSTALLFOLDER]volatus.exe" "%V"` (Background). **Deviation**: the spec's `"%1."` layer is dropped (OD2). | `%1.` gives `C:\Some Dir.`: the UI shows the dot, and child paths `C:\Some Dir.\x` are not normalised by Win32 (only the last segment loses trailing dots), so the walker could fail on every child. The parser repair alone covers the only case that breaks: a root ending in `\`. |
| Repair is pure: a folder argument with exactly one `"`, as last char, not first, gets that `"` replaced by `\`. **Deviation**: no "repaired path exists" check. | `"` is illegal in Win32 file names, so such an argument can never name a folder; the repair cannot turn a valid path into another. No file-system seam needed. |
| Repair lives in `parse_args_with` at the single point the folder is taken (`cli.rs:56`). | One entry point for every launch: `main.rs:1368` (Start menu, Explorer, Run box) and the elevated relaunch (same `main`; `elevate::quote_arg` already quotes correctly, `elevate.rs:19-41`). |
| No runtime flag for the repair. **Deviation** from golden rule 7. | Only arguments that were always invalid change. Parity test: every existing `cli.rs`/`elevate.rs` test unchanged, plus "no `"` in argument -> identity". The menu itself is gated by the installer feature. |
| Menu = WiX Registry elements in feature `ExplorerMenu` (Level 1), not a property-conditioned component. | Feature states migrate on major upgrade, so a user's "off" survives upgrades with no extra code. Checkbox `EXPLORERMENU` (default `1`, Secure) publishes `AddLocal`/`Remove ExplorerMenu` on the options page Next. |
| `util:CloseApplication Target="volatus.exe" CloseMessage="yes" RebootPrompt="no"`, rescheduled `Before="InstallValidate"` via `<Custom Action="WixCloseApplications_x64">`. No Elevated* / TerminateProcess. | wix3141rtm `UtilExtension_Platform.wxi:28` schedules it `Before="InstallFiles"` (after RemoveExistingProducts at 1501: the old exe would be locked during removal). `CloseApps.cpp:377,436`: the deferred action is scheduled only for Elevated*/RebootPrompt/Terminate, and RebootPrompt defaults to yes; a deferred action before InstallInitialize fails. Default wait 5000 ms (`CloseApps.cpp:5`). x64 id suffix `_x64` (`caSuffix.wxi`). Id must be captured from the built MSI (see Framework identifiers). |
| Static CRT (`.cargo/config.toml` `+crt-static`) and Negotium's `check-imports.ps1`. Not in the spec. | The MSI ships only `volatus.exe`; without it the exe needs the VC++ Redistributable (Negotium follow-up line 14). |
| cargo-wix 0.3.9, `[package.metadata.wix] name = "volatus"`, `linker-args = ["-sice:ICE43", "-sice:ICE57"]`. | cargo-wix compiles every `app/wix/*.wxs`, passes `-dVersion`, `-dCargoTargetBinDir`, links WixUI/WixUtil, binds with `-b app/` (registry `cargo-wix-0.3.9/src/create.rs:530-560,605-625,986`). Output `target/wix/volatus-X.Y.Z-x86_64.msi`. ICE43/57: per-machine shortcut keyed by a Program Files file (Negotium `Cargo.toml` comment). |
| One script `.github/scripts/build-msi.ps1` (WiX download + SHA-256, cargo-wix install, `cargo build --release -p app`, imports check, `cargo wix -p app --no-build --nocapture -b $WIX`, `check-msi.ps1`) used by CI and release. | CI and release cannot drift; the owner can test a PR's MSI artifact before any tag. |
| `check-msi.ps1` reads the built MSI with the `WindowsInstaller.Installer` COM object (read-only). | Only the built MSI shows the real CloseApplication action id and sequence numbers, and the Registry table light produced. |
| release-please `release-type: simple`, `extra-files: [{type: toml, path: app/Cargo.toml, jsonpath: $.package.version}]`, `draft: true`, `force-tag-creation: true`, `bump-minor-pre-major: true`, `include-component-in-tag: false`; manifest `{}`; `release-as: "0.1.0"`; top-level `bootstrap-sha` = main's SHA at P4. | `rust` type rewrites the root `Cargo.toml`, which throws on a workspace root with no `[package]` (release-please `updaters/rust/cargo-toml.ts`). `release-as` must be removed after v0.1.0 (P6 step). `Cargo.lock` keeps the old member version until the next local build (OD4). |
| UI flow (Negotium's): WelcomeDlg -> LicenseAgreementDlg (`app/wix/License.rtf`, MIT text) -> OptionsDlg (menu checkbox) -> VerifyReadyDlg; upgrades and maintenance skip License and Options. Exit dialog "Launch Volatus" via `WixShellExec` (`WixShellExecTarget=[#VolatusExe]`), unticked by default, `NOT Installed`. | ExitDialog actions run in the client process, so a double-clicked MSI starts Volatus unelevated (M4 checks). |
| Launch condition `Installed OR VOLATUS_WIN10` where `VOLATUS_WIN10` = RegistrySearch HKLM 64-bit `SOFTWARE\Microsoft\Windows NT\CurrentVersion` `CurrentMajorVersionNumber` (raw). 32-bit Windows is refused by `Platform="x64"` itself. | MSI's VersionNT is 603 on 8.1, 10 and 11; the value exists from Windows 10 on (assumption: absence on 8.1 not testable here, stated exemption). ARM64: OD1. |

## Framework identifiers (capture before trusting tests)
- CloseApplication action id in the x64 MSI (expected `WixCloseApplications_x64`) and that no `WixCloseApplicationsDeferred*` row exists: capture from the built MSI's `InstallExecuteSequence`/`CustomAction` tables in P3.T4 (log to `.claude/work/storagetool-wiztree-clone/logs/I-P3-msi-tables.txt`); `check-msi.ps1` asserts the captured id.
- Explorer `%1` for a drive is `C:\` and argv is `C:"`: confirmed by `CommandLineToArgvW` in tests (existing helper `elevate.rs:88-105`); real Explorer behaviour by M5-M7.

## Failure behaviour
| Dependency | Not configured / failed | Direction |
|---|---|---|
| Running Volatus at upgrade/uninstall | not running: nothing / does not close in 5 s (elevated, other session): Windows Files in Use dialog | degrade to asking the user; never kill; `REBOOT` not forced |
| WiX download in CI | hash mismatch or missing candle/light | fail the job loudly; no MSI |
| Tag vs `app` version | mismatch | fail before build; draft stays unpublished |
| MSI build in release | any step fails | draft release stays a draft with no asset; owner fixes and cuts a new release |
| Launch condition registry value | absent | refuse install with "Volatus needs 64-bit Windows 10 or newer." |

## Files
| File | Change |
|---|---|
| `app/src/cli.rs` | `fn repair_folder(arg: String) -> String`; used at `:56`; tests. |
| `app/src/elevate.rs` | Test module `pub(crate)`, `argv` `pub(crate)` (reuse, no copy). |
| `app/Cargo.toml` | dev-dep `roxmltree = "0.21.1"`; `[package.metadata.wix]`. |
| `.cargo/config.toml` (new) | `[target.x86_64-pc-windows-msvc] rustflags = ["-C","target-feature=+crt-static"]`. |
| `app/wix/main.wxs`, `app/wix/License.rtf` (new) | Installer source; MIT text as RTF. |
| `app/tests/wix.rs` (new) | Static checks on `main.wxs`. |
| `.github/workflows/ci.yml` (new) | test job; msi job. |
| `.github/workflows/release-please.yml` (new) | release-please, build-msi, publish-msi. |
| `.github/scripts/{build-msi,check-imports,check-msi}.ps1` (new) | Shared build; Negotium's import check copied; MSI table check. |
| `.github/dependabot.yml` (new) | github-actions weekly (Negotium's). |
| `release-please-config.json`, `.release-please-manifest.json` (new) | As decided. |
| `README.md`, `docs/USAGE.md`, `docs/RELEASING.md` (new) | Install, Explorer menu, release steps. |

## Contracts
- `fn repair_folder(arg: String) -> String` (private, `cli.rs`). Returns `arg` with its only `"` replaced by `\` when that `"` is the last char and `arg.len() > 1`; otherwise `arg` unchanged. Applied to the folder in both branches (before and after `--`). `parse_args`/`parse_args_with` signatures unchanged.
- Registry contract (per key K in `Directory\shell`, `Directory\Background\shell`, `Drive\shell`): `HKLM\Software\Classes\K\Volatus` default = `Scan with Volatus`, `Icon` = `[INSTALLFOLDER]volatus.exe,0`; `...\Volatus\command` default = `"[INSTALLFOLDER]volatus.exe" "%1"` (`"%V"` for Background). Key `...\Volatus` has `ForceCreateOnInstall`/`ForceDeleteOnUninstall="yes"`. Fixed component GUIDs.
- `build-msi.ps1 -Version <X.Y.Z>` -> exactly one `target/wix/volatus-<X.Y.Z>-x86_64.msi`, path written to `$env:GITHUB_ENV` as `MSI_PATH` when present; non-zero exit on any failure.
- `check-msi.ps1 <msi> <X.Y.Z>`: throws unless ProductVersion = X.Y.Z, UpgradeCode = the fixed one, close action seq < InstallValidate < InstallInitialize < RemoveExistingProducts < InstallFiles, no deferred close action, every Registry row Root=2 and key under the three `\Volatus` paths.

## Test Plan
`app/src/cli.rs` (unit; P1)
- `C:"` -> path `C:\`; `D:"` -> `D:\`
- `C:\Some Dir"` -> `C:\Some Dir\`; `C:\caf\u{e9} \u{4e2d}\u{6587}"` -> same with `\`; `\\server\share"` -> `\\server\share\`
- `C:\a"b` (quote in the middle) unchanged; lone `"` unchanged; `C:""` (two quotes) unchanged
- `["C:\"", "--timing"]` -> path `C:\`, timing on; `["--timing", "--", "C:\""]` -> path `C:\`
- real line `volatus.exe "C:\" --timing` via `argv` -> path `C:" --timing` unchanged (documented limit)
- real lines `"C:\Program Files\Volatus\volatus.exe" "<p>"` for p in {`C:\`, `D:\`, `C:\My Files`, unicode folder, `\\server\share`} -> path = p
- argument without `"` (every folder in existing tests + fuzz-free list of 10) -> identity (parity)
- all existing `cli.rs` and `elevate.rs` tests pass unchanged (parity)
`app/src/cli.rs` (unit; P3.T3, reads `include_str!("../wix/main.wxs")`)
- `menu_command_lines_round_trip_through_the_parser`: each of the three `command` values, `[INSTALLFOLDER]volatus.exe` -> `C:\Program Files\Volatus\volatus.exe`, `%1`/`%V` -> each path in the P1 list (drive roots for Drive and Background) -> `parse_args` path = the folder, no notices
`app/tests/wix.rs` (integration, roxmltree; P3)
- `the_upgrade_code_is_fixed_and_a_guid`: UpgradeCode = constant; Product Id `*`; Name Volatus; Manufacturer MyndPhreak; Version `$(var.Version)`
- `the_package_version_is_one_an_msi_can_carry`: `CARGO_PKG_VERSION` X.Y.Z digits in range
- `it_installs_per_machine_and_refuses_downgrades`: perMachine, x64, DowngradeErrorMessage non-empty, Schedule afterInstallInitialize, AllowSameVersionUpgrades yes, no AllowDowngrades, MigrateFeatures not `no`
- `the_exe_goes_to_program_files_volatus_with_a_start_menu_shortcut`: INSTALLFOLDER Name Volatus under ProgramFiles64Folder; File `volatus.exe` from `$(var.CargoTargetBinDir)`; Shortcut "Volatus" in ProgramMenuFolder, nested in the File, Advertise no
- `the_arp_entry_has_icon_links_and_no_modify`: ARPPRODUCTICON -> Icon from `assets\volatus.ico`; ARPURLINFOABOUT/ARPHELPLINK github.com/MyndPhreak/Volatus; ARPNOMODIFY 1
- `the_launch_condition_needs_windows_10`: one Condition `Installed OR VOLATUS_WIN10`; RegistrySearch Win64 HKLM key/name as decided; message text
- `the_menu_registry_matches_the_contract`: exactly 3 verb keys x (default, Icon, command) values as in Contracts; Root HKLM; Win64 yes
- `no_registry_write_outside_the_three_verb_keys`: every RegistryKey/RegistryValue/RemoveRegistry* resolved path is under one of the three `...\Volatus` keys; no HKCU/HKMU/HKCR/HKU root anywhere
- `only_the_volatus_verb_keys_are_force_deleted`: every ForceDeleteOnUninstall key path ends in `\shell\Volatus`
- `every_component_guid_is_fixed_unique_and_a_guid`: non-file components have literal GUIDs, all distinct; each component referenced by exactly one feature
- `the_menu_is_a_feature_on_by_default_offered_on_first_install_only`: Feature ExplorerMenu Level 1 holds exactly the 3 menu components; Property EXPLORERMENU Value 1 Secure yes; one CheckBox for it, text "Add Scan with Volatus to the Explorer menu"; OptionsDlg reached only with `NOT Installed AND NOT WIX_UPGRADE_DETECTED`; Next publishes AddLocal (EXPLORERMENU) and Remove (NOT EXPLORERMENU)
- `a_running_copy_is_asked_to_close_before_files_are_checked`: CloseApplication Target volatus.exe, CloseMessage yes, RebootPrompt no, no Elevated*/TerminateProcess; Custom `WixCloseApplications_x64` Before InstallValidate
- `launch_volatus_is_offered_unticked_on_the_last_page`: WIXUI_EXITDIALOGOPTIONALCHECKBOXTEXT "Launch Volatus"; no default for the checkbox property; WixShellExecTarget `[#VolatusExe]`; ExitDialog Finish DoAction condition `WIXUI_EXITDIALOGOPTIONALCHECKBOX = 1 AND NOT Installed`
- `the_license_page_shows_the_mit_text`: WixUILicenseRtf = `wix\License.rtf`; every non-empty line of `LICENSE` appears in the RTF
- `no_custom_action_runs_volatus`: no CustomAction with FileKey VolatusExe or ExeCommand (only WixShellExec from the dialog)
`.github/scripts/check-msi.ps1` (on the built MSI; P3.T4, CI and release): the five assertions in Contracts.
`.github/scripts/check-imports.ps1`: no vcruntime/msvcp/ucrtbase/api-ms-win-crt imports.
**Exemptions** (stated): workflow YAML (covered by `actionlint` if installable, else the first CI run on the owner-pushed branch, P6 step 3); the real install, upgrade, uninstall, UAC and Explorer behaviour (manual M1-M14); Windows 8.1 refusal (no 8.1 machine; stated assumption).

**Manual checks** (Windows Sandbox or a VM for M2; the owner's PC for the rest; record results in the P6 report):
- M1 Install the PR-artifact MSI: one UAC prompt; `C:\Program Files\Volatus\volatus.exe` exists; Start menu "Volatus" starts it; Installed apps shows Volatus, version, MyndPhreak, icon.
- M2 In Windows Sandbox (no VC++ runtime): install, start Volatus, it opens.
- M3 Run the MSI as a standard user and click No at UAC: no folder, no Start entry, no registry keys.
- M4 Tick "Launch Volatus": Volatus starts not elevated (start page shows "Restart as administrator").
- M5 Right-click a folder with spaces and unicode (classic menu): "Scan with Volatus" with icon; it scans that folder.
- M6 Right-click C: in This PC: one entry; it scans the whole drive (top bar shows `C:\`).
- M7 Right-click the background of `C:\` and of a folder: scans that folder.
- M8 Install with the menu checkbox off: no entries. Upgrade (M9): still no entries.
- M9 Build a version +1 MSI locally (`cargo wix` with `-i 0.1.1`), run it while Volatus runs unelevated: Volatus closes, upgrade completes, no reboot prompt, one ARP entry.
- M10 Same as M9 with Volatus elevated: Files in Use dialog names Volatus; close it, Retry, upgrade completes.
- M11 Run the 0.1.0 MSI after 0.1.1: refused with the downgrade message.
- M12 Uninstall: folder, shortcut and all three menu keys gone (`reg query "HKLM\Software\Classes\Drive\shell\Volatus"` fails); `Directory\shell` still has other entries.
- M13 Run the same MSI twice: second run offers Repair/Remove; a same-version rebuild installs in place (one ARP entry).
- M14 Start the MSI twice at once: the second says another installation is in progress.

## Out of Scope
Code signing; auto-update, winget, Store, portable zip; Windows 11 short menu; menu on files; turning the menu on/off after install (reinstall needed; follow-up); single-instance/IPC; ARM64 build; non-UTF-16-valid argument handling (`std::env::args` panics, `main.rs:1368`; follow-up); Cargo.lock version sync (OD4); renaming package `app`.

## Definition of Done
- [ ] `C:"` and `C:\Dir"` folder arguments scan `C:\` and `C:\Dir\` (P1 tests)
- [ ] Arguments without a quote parse exactly as before (P1 parity tests)
- [ ] CI runs clippy `-D warnings` and workspace tests on windows-latest for PRs and main (P2; P6 step 3 first green run)
- [ ] CI builds an MSI artifact on every PR, imports check and `check-msi.ps1` pass (P3.T4; P6 step 3)
- [ ] `.wxs` static tests all pass (P3)
- [ ] Menu command lines round-trip to the right folder (`menu_command_lines_round_trip_through_the_parser`)
- [ ] No registry write outside the three verb keys; uninstall removes them (wix tests; M12)
- [ ] Upgrade closes an unelevated running copy without reboot; downgrade refused (wix tests, check-msi; M9-M11)
- [ ] Release workflow: tag == `app` version check, pinned WiX hash, read-only build job, draft published only with the MSI (P4 review + P6 step 6 dry read; first real run is the owner's merge)
- [ ] README, USAGE, RELEASING updated (P5 review)
- [ ] Release pull request open (P6 step 5)

## Acceptance Criteria
1. A user downloads the MSI from the Release, installs with one UAC prompt, and starts Volatus from Start. (DoD 4, M1)
2. Right-click on a folder, folder background or drive starts a scan of exactly that location. (DoD 1, 6; M5-M7)
3. Upgrading replaces the installed version in place and closes or asks about a running copy; downgrading is refused. (DoD 8)
4. Uninstalling leaves no program file, shortcut or menu entry. (DoD 7)
5. Merging the release pull request publishes a release with the MSI and no manual build. (DoD 9, 11)

## Phases and Task Breakdown
Commit style: `Add failing tests for <area>` then `Implement <area>`; conventional titles only on the squash-merge (owner).

**P1 Parser repair** (sonnet/medium)
| Task | Behaviour | Files | Tests | Risk |
|---|---|---|---|---|
| P1.T1 | Folder argument with a stray trailing quote is repaired | `app/src/cli.rs`, `app/src/elevate.rs` (argv pub(crate)) | cli.rs P1 lines | none |
- [ ] Red: stub `repair_folder` returning `arg` unchanged, wire at `:56`; tests fail on `C:"` assertions. Green: implement. Verify `cargo test -p app cli::` then workspace + clippy.

**P2 CI** (sonnet/medium)
| P2.T1 | Lint and test on Windows for PRs and main | `.github/workflows/ci.yml`, `.github/dependabot.yml` | exemption: actionlint / first run | none |
- [ ] Mirror Negotium `ci.yml` (checkout `persist-credentials: false`, rust-toolchain stable + clippy, rust-cache `save-if` main only, `permissions: contents: read`, concurrency). No `--skip` (Volatus has no known flaky test; `#[ignore]` already skips the elevated ones). Pin every action to a commit SHA with a version comment (`gh api repos/<o>/<r>/commits/<tag> --jq .sha`). One commit `Add CI on Windows`.

**P3 MSI** (opus/high)
| P3.T1 | Exe needs no VC++ runtime | `.cargo/config.toml`, `.github/scripts/check-imports.ps1` | imports check on `target/release/volatus.exe` (red: run before config, fails; green after) | full rebuild |
| P3.T2 | Core product: per-machine, upgrades, shortcut, ARP, launch condition, license | `app/wix/main.wxs`, `License.rtf`, `app/tests/wix.rs`, `app/Cargo.toml` | wix.rs lines 1-6, 14, 15 | GUID generation once |
| P3.T3 | Explorer menu feature | `main.wxs`, `wix.rs`, `cli.rs` | wix.rs lines 7-11; round-trip | registry path typo |
| P3.T4 | Close running copy, Launch Volatus, MSI build in CI | `main.wxs`, `wix.rs`, `build-msi.ps1`, `check-msi.ps1`, `ci.yml` (msi job, artifact 7 days) | wix.rs 12, 13; check-msi; identifier capture | `_x64` id |
- [ ] Red commit of P3.T2 carries a minimal compiling `main.wxs` (Product/Package only) so tests fail on assertions.
- [ ] After P3.T4: `pwsh .github/scripts/build-msi.ps1 -Version 0.1.0` locally -> one MSI, both checks print OK; save tables to the logs path above.

**P4 Release** (opus/high)
| P4.T1 | Release PR, tag check, MSI attach, publish | `release-please-config.json`, `.release-please-manifest.json`, `.github/workflows/release-please.yml` | exemption: actionlint + review against Negotium `release-please.yml` | `release-as` left in |
- [ ] Mirror Negotium's three jobs: `permissions: {}` top; release-please (contents+PRs write); build-msi (contents read, checkout the tag, tag check selecting the package with bin `volatus`, `build-msi.ps1`, upload artifact 1 day); publish-msi (contents write, exactly one MSI, `gh release upload` then `--draft=false`). `bootstrap-sha` = `git rev-parse origin/main` at P4 time.

**P5 Docs** (sonnet/medium)
| P5.T1 | Install, menu and release docs | `README.md`, `docs/USAGE.md`, `docs/RELEASING.md` | review; ASD-STE100 | none |
- [ ] README: Install section (download, SmartScreen "More info, Run anyway", per-machine, menu checkbox, uninstall), Build section stays. USAGE: "Explorer menu" section and the trailing-quote note. RELEASING: squash-merge conventional titles, release PR CI needs close/reopen, remove `release-as` after v0.1.0, what to do when the MSI build fails.

**P6 Owner steps** (chief)
1. **OWNER GATE** Repository settings: Actions may create pull requests; squash merge with PR title as the default message.
2. **OWNER GATE** Push `feat/volatus-installer`, open the PR (conventional title `feat: installer, releases and Explorer menu`).
3. Read the first CI run (test + msi jobs); fix forward if red.
4. Owner runs M1-M14 with the PR's MSI artifact.
5. **OWNER GATE** Squash-merge; release-please opens the release PR (`chore(main): release 0.1.0`); close/reopen it to run CI.
6. **OWNER GATE** Merging the release PR is the first release. Then a `chore:` PR removes `release-as` and `bootstrap-sha`.

## Dispatch advice
P1, P2, P5: sonnet builder. P3, P4: opus/high builder; one verifier (opus/high) after P3 and after P4 checking the Review Focus items against the diff and the built MSI tables.

## Open Decisions
- **OD1 ARM64.** Options: refuse (needs an MSI property not verified here) / allow under x64 emulation, README says "not tested". Recommend allow. Cost if wrong: an ARM user hits an untested build.
- **OD2 Menu `"%1."` layer dropped.** Options: keep spec's `%1.` + repair / repair only. Recommend repair only (no trailing-dot paths). Cost if wrong: none for drives; menu depends on the parser alone.
- **OD3 MSI on every PR.** Options: every PR (spec) / only on main. Recommend every PR (gives the owner an artifact to test). Cost: about 5-10 extra minutes per PR.
- **OD4 Cargo.lock version.** Options: accept drift (Negotium) / bump via a jsonpath filter (unverified in release-please's jsonpath-plus use). Recommend accept. Cost: one-line lock change in the next PR after each release.

## Self-review
- Assumptions left open: `CurrentMajorVersionNumber` absent on 8.1; WM_CLOSE blocked to an elevated copy (M10 covers both outcomes); `Icon` value format `[INSTALLFOLDER]volatus.exe,0` (M5).
- `MigrateFeatures` keeping "menu off" on upgrade is runtime behaviour: M8 only.
- Spec items changed: OD2, no exists-check, package name, static CRT added, no Slint UI tests exist in `app` (spec goal 2 line is moot).
