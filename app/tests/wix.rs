//! Static checks on the installer's WiX source (`wix/main.wxs`): what it installs where, the fixed
//! UpgradeCode, upgrades, the launch condition and the license page. The MSI itself is built by
//! `.github/scripts/build-msi.ps1` and its tables are checked by `.github/scripts/check-msi.ps1`;
//! these checks never build, install or run it.
use roxmltree::{Document, Node};
use std::path::{Path, PathBuf};

const WIX_NS: &str = "http://schemas.microsoft.com/wix/2006/wi";
/// Fixed for the life of the product: every release's MSI must carry it, or upgrades install
/// side by side instead of replacing the old version.
const UPGRADE_CODE: &str = "FB5B570F-DD29-49B8-A6AE-35855278CAE6";
/// The folder of `main.wxs` as candle sees it (`$(sys.SOURCEFILEDIR)`, with its trailing `\`).
const SOURCE_DIR: &str = "$(sys.SOURCEFILEDIR)";

fn wix_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("wix")
}

fn source() -> String {
    let path = wix_dir().join("main.wxs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn is(node: &Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name && node.tag_name().namespace() == Some(WIX_NS)
}

fn all<'a>(doc: &'a Document, name: &str) -> Vec<Node<'a, 'a>> {
    doc.descendants().filter(|n| is(n, name)).collect()
}

fn one<'a>(doc: &'a Document, name: &str) -> Node<'a, 'a> {
    let found = all(doc, name);
    assert_eq!(found.len(), 1, "exactly one <{name}>");
    found[0]
}

fn attr<'a>(node: &Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attribute(name)
}

/// The one `<{name}>` with this `Id`.
fn by_id<'a>(doc: &'a Document, name: &str, id: &str) -> Node<'a, 'a> {
    let found: Vec<Node> = all(doc, name).into_iter().filter(|n| attr(n, "Id") == Some(id)).collect();
    assert_eq!(found.len(), 1, "exactly one <{name} Id=\"{id}\">");
    found[0]
}

/// The `Value` of `<Property Id="{id}">`, which must exist.
fn property<'a>(doc: &'a Document, id: &str) -> Option<&'a str> {
    attr(&by_id(doc, "Property", id), "Value")
}

fn is_guid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12]) && groups.iter().all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A source path in `main.wxs` as a real path: `$(sys.SOURCEFILEDIR)` is the `wix` folder.
fn resolve(source_path: &str) -> PathBuf {
    let rest = source_path
        .strip_prefix(SOURCE_DIR)
        .unwrap_or_else(|| panic!("{source_path:?}: relative to {SOURCE_DIR}, so it does not depend on the working folder"));
    wix_dir().join(rest.replace('\\', "/"))
}

/// A condition with its whitespace normalised.
fn condition(node: &Node) -> String {
    node.text().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn the_upgrade_code_is_fixed_and_a_guid() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let product = one(&doc, "Product");
    let code = attr(&product, "UpgradeCode").expect("UpgradeCode");
    assert!(is_guid(code), "{code}");
    assert_eq!(code, UPGRADE_CODE, "the UpgradeCode never changes");
    assert_eq!(attr(&product, "Id"), Some("*"), "a new ProductCode each build: every release is a major upgrade");
    assert_eq!(attr(&product, "Name"), Some("Volatus"));
    assert_eq!(attr(&product, "Manufacturer"), Some("MyndPhreak"));
    assert_eq!(attr(&product, "Version"), Some("$(var.Version)"), "the app package's version (cargo-wix)");
}

/// The MSI's ProductVersion is the app package's version: Windows Installer takes only
/// `major.minor.build` numbers (major and minor up to 255, build up to 65535), so a pre-release
/// (`0.2.0-beta.1`) or larger number fails here, plainly, before any installer is built.
#[test]
fn the_package_version_is_one_an_msi_can_carry() {
    let version = env!("CARGO_PKG_VERSION");
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(parts.len(), 3, "{version}: X.Y.Z only");
    let numbers: Vec<u32> = parts
        .iter()
        .map(|p| {
            assert!(!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()), "{version}: digits only (no pre-release)");
            p.parse().unwrap()
        })
        .collect();
    assert!(numbers[0] <= 255 && numbers[1] <= 255 && numbers[2] <= 65535, "{version}: out of an MSI version's range");
}

#[test]
fn it_installs_per_machine_and_refuses_downgrades() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let package = one(&doc, "Package");
    assert_eq!(attr(&package, "InstallScope"), Some("perMachine"));
    assert_eq!(attr(&package, "InstallPrivileges"), Some("elevated"));
    assert_eq!(attr(&package, "Platform"), Some("x64"));
    let upgrade = one(&doc, "MajorUpgrade");
    assert!(attr(&upgrade, "DowngradeErrorMessage").is_some_and(|m| !m.trim().is_empty()), "downgrades are refused with a message");
    assert!(attr(&upgrade, "AllowDowngrades").is_none());
    // Inside the install transaction: a failed upgrade rolls back to the old version.
    assert_eq!(attr(&upgrade, "Schedule"), Some("afterInstallInitialize"));
    // A rebuild of the same version replaces the installed one instead of installing beside it.
    assert_eq!(attr(&upgrade, "AllowSameVersionUpgrades"), Some("yes"));
    // The Explorer menu feature keeps its state (on or off) across upgrades.
    assert_ne!(attr(&upgrade, "MigrateFeatures"), Some("no"));
}

#[test]
fn the_exe_goes_to_program_files_volatus_with_a_start_menu_shortcut() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let folder = by_id(&doc, "Directory", "INSTALLFOLDER");
    assert_eq!(attr(&folder, "Name"), Some("Volatus"));
    assert_eq!(folder.parent().and_then(|p| attr(&p, "Id")), Some("ProgramFiles64Folder"));
    let exe = by_id(&doc, "File", "VolatusExe");
    assert_eq!(attr(&exe, "Name"), Some("volatus.exe"));
    assert_eq!(attr(&exe, "Source"), Some(r"$(var.CargoTargetBinDir)\volatus.exe"));
    assert_eq!(attr(&exe, "KeyPath"), Some("yes"), "the exe is its component's key path");
    assert!(exe.ancestors().any(|a| is(&a, "Directory") && attr(&a, "Id") == Some("INSTALLFOLDER")));
    // Nested in the exe's File: it targets that file; not advertised, so it starts the exe itself.
    let shortcut = one(&doc, "Shortcut");
    assert_eq!(attr(&shortcut, "Name"), Some("Volatus"));
    assert_eq!(shortcut.parent().and_then(|p| attr(&p, "Id")), Some("VolatusExe"));
    assert_eq!(attr(&shortcut, "Directory"), Some("ProgramMenuFolder"));
    assert_eq!(attr(&shortcut, "Advertise"), Some("no"));
}

#[test]
fn the_arp_entry_has_icon_links_and_no_modify() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let icon_id = property(&doc, "ARPPRODUCTICON").expect("ARPPRODUCTICON has a value");
    let icon = by_id(&doc, "Icon", icon_id);
    let file = attr(&icon, "SourceFile").expect("Icon SourceFile");
    assert!(file.ends_with(r"assets\volatus.ico"), "{file}: the exe's own icon");
    assert!(resolve(file).is_file(), "{} exists", resolve(file).display());
    assert_eq!(property(&doc, "ARPURLINFOABOUT"), Some("https://github.com/MyndPhreak/Volatus"));
    assert_eq!(property(&doc, "ARPHELPLINK"), Some("https://github.com/MyndPhreak/Volatus/issues"));
    // Nothing to change after install: the Explorer menu is chosen at first install only.
    assert_eq!(property(&doc, "ARPNOMODIFY"), Some("1"));
}

/// `VersionNT` is 603 on Windows 8.1, 10 and 11, so it can't tell them apart; the
/// `CurrentMajorVersionNumber` value exists from Windows 10 on. 32-bit Windows is refused by the
/// x64 package itself; ARM64 Windows runs the x64 build under emulation (allowed, not tested).
#[test]
fn the_launch_condition_needs_windows_10() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let product = one(&doc, "Product");
    let conditions: Vec<Node> = product.children().filter(|n| is(n, "Condition")).collect();
    assert_eq!(conditions.len(), 1, "one launch condition");
    assert_eq!(condition(&conditions[0]), "Installed OR VOLATUS_WIN10");
    assert_eq!(attr(&conditions[0], "Message"), Some("Volatus needs 64-bit Windows 10 or newer."));
    let win10 = by_id(&doc, "Property", "VOLATUS_WIN10");
    assert!(attr(&win10, "Value").is_none(), "set only by the search");
    let searches: Vec<Node> = win10.children().filter(|n| is(n, "RegistrySearch")).collect();
    assert_eq!(searches.len(), 1);
    let search = searches[0];
    assert_eq!(attr(&search, "Root"), Some("HKLM"));
    assert_eq!(attr(&search, "Win64"), Some("yes"), "the 64-bit registry view");
    assert_eq!(attr(&search, "Key"), Some(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"));
    assert_eq!(attr(&search, "Name"), Some("CurrentMajorVersionNumber"));
    assert_eq!(attr(&search, "Type"), Some("raw"));
}

#[test]
fn the_license_page_shows_the_mit_text() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let rtf = attr(&by_id(&doc, "WixVariable", "WixUILicenseRtf"), "Value").expect("WixUILicenseRtf value");
    assert_eq!(rtf, format!("{SOURCE_DIR}License.rtf"), "next to main.wxs");
    let rtf = std::fs::read_to_string(resolve(rtf)).unwrap_or_default();
    assert!(rtf.starts_with(r"{\rtf1"), "an RTF file");
    let license_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../LICENSE");
    let license = std::fs::read_to_string(&license_path).unwrap_or_else(|e| panic!("{}: {e}", license_path.display()));
    for line in license.lines().map(str::trim).filter(|l| !l.is_empty()) {
        assert!(rtf.contains(line), "License.rtf has the LICENSE line {line:?}");
    }
    // The page is on the first-install path and needs the box ticked to go on.
    let to_license: Vec<Node> = all(&doc, "Publish")
        .into_iter()
        .filter(|p| attr(p, "Dialog") == Some("WelcomeDlg") && attr(p, "Event") == Some("NewDialog"))
        .filter(|p| attr(p, "Value") == Some("LicenseAgreementDlg"))
        .collect();
    assert_eq!(to_license.len(), 1, "Welcome leads to the license page");
    let next: Vec<Node> = all(&doc, "Publish")
        .into_iter()
        .filter(|p| attr(p, "Dialog") == Some("LicenseAgreementDlg") && attr(p, "Control") == Some("Next"))
        .collect();
    assert!(!next.is_empty() && next.iter().all(|p| condition(p).contains(r#"LicenseAccepted = "1""#)), "Next needs the license accepted");
}

/// The installer never runs `volatus.exe` to do its work (no custom action to fail). The only
/// custom action is "Launch Volatus", which the last page runs through WixShellExec.
#[test]
fn no_custom_action_runs_volatus() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    for action in all(&doc, "CustomAction") {
        let id = attr(&action, "Id").unwrap_or_default();
        for runs in ["FileKey", "ExeCommand", "Script", "JScriptCall", "VBScriptCall"] {
            assert!(attr(&action, runs).is_none(), "{id} has {runs}");
        }
        if attr(&action, "DllEntry").is_some() {
            assert_eq!((attr(&action, "BinaryKey"), attr(&action, "DllEntry")), (Some("WixCA"), Some("WixShellExec")), "{id}");
        }
    }
    for sequence in ["InstallExecuteSequence", "InstallUISequence", "AdminExecuteSequence", "AdminUISequence"] {
        for custom in all(&doc, sequence).iter().flat_map(|s| s.children().filter(|n| is(n, "Custom"))) {
            let id = attr(&custom, "Action").unwrap_or_default();
            assert!(id.starts_with("WixCloseApplications"), "{sequence} runs only WiX's own close step, not {id}");
        }
    }
}

/// Uninstall removes everything the install made: no component is kept, and the shortcut sits in
/// the Start menu folder itself (no folder of its own to leave behind).
#[test]
fn uninstall_leaves_nothing_behind() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let components = all(&doc, "Component");
    assert!(!components.is_empty(), "the product has components");
    for component in &components {
        let id = attr(component, "Id").unwrap_or_default();
        assert_ne!(attr(component, "Permanent"), Some("yes"), "{id} is removed on uninstall");
        assert_ne!(attr(component, "NeverOverwrite"), Some("yes"), "{id}");
        assert_ne!(attr(component, "Guid"), Some(""), "{id}: an empty GUID is never removed");
    }
    let menu_folder = by_id(&doc, "Directory", "ProgramMenuFolder");
    assert!(!menu_folder.children().any(|n| is(&n, "Directory")), "no Start menu subfolder");
}
