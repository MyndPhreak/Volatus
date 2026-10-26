//! Static checks on the installer's WiX source (`wix/main.wxs`): what it installs where, the fixed
//! UpgradeCode, upgrades, the launch condition, the license page and the Explorer menu's registry
//! keys. The MSI itself is built by `.github/scripts/build-msi.ps1` and its tables are checked by
//! `.github/scripts/check-msi.ps1`; these checks never build, install or run it, and never touch
//! the registry.
use roxmltree::{Document, Node};
use std::collections::BTreeSet;
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

/// A condition's `AND`ed clauses. An `OR` would need real parsing, so it fails the test instead.
fn clauses(node: &Node) -> BTreeSet<String> {
    let condition = condition(node);
    assert!(!condition.contains(" OR "), "{condition:?}: only conjunctions are checked");
    condition.split(" AND ").map(str::to_string).filter(|c| !c.is_empty()).collect()
}

fn set(parts: &[&str]) -> BTreeSet<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

const FIRST_INSTALL: [&str; 2] = ["NOT Installed", "NOT WIX_UPGRADE_DETECTED"];

/// The Explorer menu: one verb key per place, each a component with a fixed GUID (a component's
/// GUID must stay the same for as long as its key path does). `%1` is the folder or drive that was
/// right-clicked; `%V` is the folder whose background was right-clicked.
const MENU: [(&str, &str, &str, &str); 3] = [
    ("MenuDirectory", "41C7BA3F-1D39-478A-9629-C08E1CBCD053", r"Software\Classes\Directory\shell\Volatus", "%1"),
    ("MenuBackground", "D1D6A47D-523E-4BCD-9A37-4B6E4BF365D8", r"Software\Classes\Directory\Background\shell\Volatus", "%V"),
    ("MenuDrive", "C057D819-1438-463A-BCFC-19B8B087E8D4", r"Software\Classes\Drive\shell\Volatus", "%1"),
];

/// The exe as the menu's values name it: the install folder (with its trailing `\`) and the file
/// name, not `[#VolatusExe]`, which ICE69 refuses across features.
const MENU_EXE: &str = "[INSTALLFOLDER]volatus.exe";

/// Every element that writes or removes registry data.
const REGISTRY_WRITES: [&str; 5] = ["RegistryKey", "RegistryValue", "RemoveRegistryKey", "RemoveRegistryValue", "Registry"];

/// The root and full key of a registry element: its own `Key` under each enclosing
/// `<RegistryKey Key>`, and the nearest `Root`.
fn registry_path(node: &Node) -> (String, String) {
    let root = node.ancestors().find_map(|a| attr(&a, "Root")).unwrap_or_default().to_string();
    let mut keys: Vec<&str> = node
        .ancestors()
        .filter(|a| REGISTRY_WRITES.iter().any(|w| is(a, w)))
        .filter_map(|a| attr(&a, "Key"))
        .collect();
    keys.reverse();
    (root, keys.join(r"\"))
}

/// The component that holds `node`.
fn component<'a>(node: &Node<'a, 'a>) -> Node<'a, 'a> {
    node.ancestors().find(|a| is(a, "Component")).expect("inside a Component")
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

// ---- Explorer menu ----

/// Each verb key holds its label and the exe's icon; its `command` key starts the installed exe
/// with the folder as the only argument. Nothing else is written.
#[test]
fn the_menu_registry_matches_the_contract() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let found: BTreeSet<(String, String, String, String, String)> = all(&doc, "RegistryValue")
        .iter()
        .map(|v| {
            let (root, key) = registry_path(v);
            let name = attr(v, "Name").unwrap_or_default().to_string();
            (root, key, name, attr(v, "Value").unwrap_or_default().to_string(), attr(v, "Type").unwrap_or_default().to_string())
        })
        .collect();
    let value = |key: String, name: &str, value: String| ("HKLM".to_string(), key, name.to_string(), value, "string".to_string());
    let mut want = BTreeSet::new();
    for (_, _, key, arg) in MENU {
        want.insert(value(key.to_string(), "", "Scan with Volatus".to_string()));
        want.insert(value(key.to_string(), "Icon", format!("{MENU_EXE},0")));
        want.insert(value(format!(r"{key}\command"), "", format!(r#""{MENU_EXE}" "{arg}""#)));
    }
    assert_eq!(found, want);
    assert_eq!(all(&doc, "RegistryValue").len(), 9, "no value twice");
    for v in all(&doc, "RegistryValue") {
        assert_eq!(attr(&component(&v), "Win64"), Some("yes"), "the 64-bit registry view");
    }
}

/// Uninstall removes these keys, and nothing outside them is written: no HKCU (other users would
/// keep a menu entry naming a removed exe), no HKCR (it maps to HKCU for elevated writes too).
#[test]
fn no_registry_write_outside_the_three_verb_keys() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let allowed: Vec<&str> = MENU.iter().map(|(_, _, key, _)| *key).collect();
    let writes: Vec<Node> = REGISTRY_WRITES.iter().flat_map(|w| all(&doc, w)).collect();
    assert!(writes.len() >= 3, "the menu keys are written");
    for write in writes {
        let (root, key) = registry_path(&write);
        assert_eq!(root, "HKLM", "{key}");
        assert!(allowed.iter().any(|a| key == *a || key.starts_with(&format!(r"{a}\"))), "{key} is outside the menu keys");
    }
    for node in doc.descendants().filter(|n| n.is_element()) {
        if let Some(root) = attr(&node, "Root") {
            assert_eq!(root, "HKLM", "<{}> uses {root}", node.tag_name().name());
        }
    }
}

/// `ForceDeleteOnUninstall` removes a key with everything under it, so it may only sit on the
/// `...\shell\Volatus` keys, never on `...\shell` (other programs' verbs live there).
#[test]
fn only_the_volatus_verb_keys_are_force_deleted() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let forced: BTreeSet<String> = all(&doc, "RegistryKey")
        .iter()
        .filter(|k| attr(k, "ForceDeleteOnUninstall") == Some("yes"))
        .map(|k| registry_path(k).1)
        .collect();
    let want: BTreeSet<String> = MENU.iter().map(|(_, _, key, _)| key.to_string()).collect();
    assert_eq!(forced, want);
    for key in &forced {
        assert!(key.ends_with(r"\shell\Volatus"), "{key}");
    }
    for key in all(&doc, "RegistryKey").iter().filter(|k| want.contains(&registry_path(k).1)) {
        assert_eq!(attr(key, "ForceCreateOnInstall"), Some("yes"), "{}", registry_path(key).1);
    }
}

#[test]
fn every_component_guid_is_fixed_unique_and_a_guid() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let mut guids = BTreeSet::new();
    for component in all(&doc, "Component") {
        let id = attr(&component, "Id").expect("Component Id");
        let guid = attr(&component, "Guid").expect("Component Guid");
        if component.children().any(|n| is(&n, "File")) {
            // WiX derives a file component's GUID from its key path, which never changes.
            assert_eq!(guid, "*", "{id}");
        } else {
            assert!(is_guid(guid), "{id}: {guid}");
            assert!(guids.insert(guid.to_uppercase()), "{id}: {guid} is used twice");
        }
        // Exactly one key path: it tells Windows Installer whether the component is installed.
        let key_paths = component.descendants().filter(|n| n.is_element() && attr(n, "KeyPath") == Some("yes")).count();
        assert_eq!(key_paths, 1, "{id}: one key path");
        let refs: Vec<Node> = all(&doc, "ComponentRef").into_iter().filter(|r| attr(r, "Id") == Some(id)).collect();
        assert_eq!(refs.len(), 1, "{id} is in exactly one feature");
        assert!(refs[0].parent().is_some_and(|p| is(&p, "Feature")), "{id}");
    }
    for (id, guid, key, _) in MENU {
        let component = by_id(&doc, "Component", id);
        assert_eq!(attr(&component, "Guid"), Some(guid), "{id}: the GUID never changes");
        let key_path = component.descendants().find(|n| attr(n, "KeyPath") == Some("yes")).unwrap();
        assert!(is(&key_path, "RegistryValue") && attr(&key_path, "Name").is_none(), "{id}: the verb's label");
        assert_eq!(registry_path(&key_path).1, key, "{id}");
    }
}

/// The menu is its own feature, on by default: Windows Installer keeps a feature's state across
/// major upgrades, so "off" survives an upgrade with no code. The checkbox is on the options page,
/// shown on a first install only.
#[test]
fn the_menu_is_a_feature_on_by_default_offered_on_first_install_only() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let feature = by_id(&doc, "Feature", "ExplorerMenu");
    assert_eq!(attr(&feature, "Level"), Some("1"), "on by default");
    assert_ne!(attr(&feature, "Absent"), Some("disallow"), "it can be left out");
    let refs: BTreeSet<&str> = feature.children().filter(|n| is(n, "ComponentRef")).filter_map(|r| attr(&r, "Id")).collect();
    assert_eq!(refs, MENU.iter().map(|(id, _, _, _)| *id).collect(), "the three menu components, nothing else");

    let checkbox = by_id(&doc, "Property", "EXPLORERMENU");
    assert_eq!(attr(&checkbox, "Value"), Some("1"), "ticked by default");
    // Public properties set in the UI only reach the elevated install when they are secure.
    assert_eq!(attr(&checkbox, "Secure"), Some("yes"));
    let boxes: Vec<Node> = all(&doc, "Control")
        .into_iter()
        .filter(|c| attr(c, "Type") == Some("CheckBox") && attr(c, "Property") == Some("EXPLORERMENU"))
        .collect();
    assert_eq!(boxes.len(), 1, "one checkbox");
    assert_eq!(attr(&boxes[0], "Text"), Some("Add Scan with Volatus to the Explorer menu"));
    assert_eq!(attr(&boxes[0], "CheckBoxValue"), Some("1"));
    assert!(boxes[0].ancestors().any(|a| is(&a, "Dialog") && attr(&a, "Id") == Some("OptionsDlg")));

    let publish = all(&doc, "Publish");
    let to_options: Vec<&Node> = publish
        .iter()
        .filter(|p| attr(p, "Event") == Some("NewDialog") && attr(p, "Value") == Some("OptionsDlg"))
        .collect();
    assert!(!to_options.is_empty(), "the options page is reachable");
    for p in to_options {
        assert!(clauses(p).is_superset(&set(&FIRST_INSTALL)), "{:?} -> OptionsDlg: first install only", attr(p, "Dialog"));
    }

    let next: Vec<&Node> = publish.iter().filter(|p| attr(p, "Dialog") == Some("OptionsDlg") && attr(p, "Control") == Some("Next")).collect();
    let event = |name: &str| -> Vec<(String, String, u32)> {
        next.iter()
            .filter(|p| attr(p, "Event") == Some(name))
            .map(|p| (attr(p, "Value").unwrap_or_default().to_string(), condition(p), attr(p, "Order").map_or(0, |o| o.parse().unwrap())))
            .collect()
    };
    let (add, remove, new_dialog) = (event("AddLocal"), event("Remove"), event("NewDialog"));
    let shown = |events: &[(String, String, u32)]| events.iter().map(|(v, c, _)| (v.clone(), c.clone())).collect::<Vec<_>>();
    assert_eq!(shown(&add), [("ExplorerMenu".to_string(), "EXPLORERMENU".to_string())]);
    assert_eq!(shown(&remove), [("ExplorerMenu".to_string(), "NOT EXPLORERMENU".to_string())]);
    assert_eq!(new_dialog.len(), 1, "Next goes on to one page");
    assert!(add[0].2 < new_dialog[0].2 && remove[0].2 < new_dialog[0].2, "the choice is applied before the page changes");
}

/// The menu feature has no condition: a feature disabled by a condition (level 0) is not
/// registered, so a major upgrade has no "absent" state to carry and installs the menu again.
#[test]
fn the_menu_feature_has_no_condition() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let feature = by_id(&doc, "Feature", "ExplorerMenu");
    let conditions = feature.children().filter(|n| is(n, "Condition")).count();
    assert_eq!(conditions, 0, "no feature condition: the menu's off state must be registered to survive upgrades");
}

/// `msiexec /i volatus.msi /qn EXPLORERMENU=0` installs without the menu, and later upgrades and
/// repairs keep it off: on a first install with no feature list of its own, the property sets
/// ADDLOCAL to Main, so the menu feature is registered as absent and MajorUpgrade migrates that.
/// In both sequences, before costing reads ADDLOCAL. Not on an upgrade (it would stop the feature
/// states from migrating), and never over an ADDLOCAL or REMOVE from the command line or the UI.
#[test]
fn explorermenu_0_on_the_command_line_installs_main_only_on_a_first_install() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let set_addlocal: Vec<Node> = all(&doc, "SetProperty").into_iter().filter(|s| attr(s, "Id") == Some("ADDLOCAL")).collect();
    assert_eq!(set_addlocal.len(), 1, "one <SetProperty Id=\"ADDLOCAL\">");
    let set_addlocal = set_addlocal[0];
    let placed = (attr(&set_addlocal, "Value"), attr(&set_addlocal, "Before"), attr(&set_addlocal, "After"), attr(&set_addlocal, "Sequence"));
    assert_eq!(placed, (Some("Main"), Some("CostInitialize"), None, Some("both")));
    assert_eq!(
        clauses(&set_addlocal),
        set(&["NOT Installed", "NOT WIX_UPGRADE_DETECTED", r#"EXPLORERMENU = "0""#, "NOT ADDLOCAL", "NOT REMOVE"])
    );
}

/// The menu's command names the exe in the install folder, so it is only right when the exe is
/// installed too: the menu feature is a child of the exe's feature, and that feature can't be
/// left out.
#[test]
fn the_menu_feature_is_always_installed_with_the_exe() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let exe_feature = component_feature(&doc, "VolatusExe");
    assert_eq!(attr(&exe_feature, "Absent"), Some("disallow"), "the exe is always installed");
    assert_eq!(attr(&exe_feature, "Level"), Some("1"));
    for (id, _, _, _) in MENU {
        let feature = component_feature(&doc, id);
        assert!(feature.ancestors().any(|a| a == exe_feature), "{id}: inside the exe's feature");
    }
}

/// ICE69 is not suppressed: the menu names the exe through `[INSTALLFOLDER]`, which ICE69 accepts,
/// so a real ICE69 finding fails the build.
#[test]
fn ice69_is_not_suppressed() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let args: Vec<&str> = manifest.lines().filter(|l| l.trim_start().starts_with("linker-args")).collect();
    assert_eq!(args.len(), 1, "one linker-args line in [package.metadata.wix]");
    assert!(!args[0].contains("ICE69"), "{}", args[0]);
}

// ---- running copy and last page ----

const UTIL_NS: &str = "http://schemas.microsoft.com/wix/UtilExtension";

/// An upgrade or uninstall asks a running Volatus to close (WM_CLOSE, then 5 s wait) before
/// InstallValidate checks for files in use, so RemoveExistingProducts and InstallFiles find the
/// exe free. No reboot prompt and no forced kill: a copy that does not close (an elevated one,
/// which WM_CLOSE from setup can't reach) gets Windows' Files in Use dialog.
#[test]
fn a_running_copy_is_asked_to_close_before_files_are_checked() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    let close: Vec<Node> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "CloseApplication" && n.tag_name().namespace() == Some(UTIL_NS))
        .collect();
    assert_eq!(close.len(), 1, "one util:CloseApplication");
    let close = close[0];
    assert_eq!(attr(&close, "Target"), Some("volatus.exe"));
    assert_eq!(attr(&close, "CloseMessage"), Some("yes"));
    // RebootPrompt defaults to yes, which schedules a deferred step at run time: that fails
    // before InstallInitialize (WiX CloseApps.cpp), and Volatus never needs a reboot.
    assert_eq!(attr(&close, "RebootPrompt"), Some("no"));
    for forced in ["ElevatedCloseMessage", "ElevatedEndSessionMessage", "EndSessionMessage", "TerminateProcess", "PromptToContinue"] {
        assert!(attr(&close, forced).is_none(), "no {forced}");
    }
    // WixUtilExtension schedules it before InstallFiles, after RemoveExistingProducts has already
    // met the locked exe; this moves it. The id as wix3141rtm's light writes it in this x64 MSI,
    // with no platform suffix (captured from the built MSI's CustomAction table).
    let sequence = one(&doc, "InstallExecuteSequence");
    let customs: Vec<Node> = sequence.children().filter(|n| is(n, "Custom")).collect();
    let moved: Vec<&Node> = customs.iter().filter(|c| attr(c, "Action") == Some("WixCloseApplications")).collect();
    assert_eq!(moved.len(), 1, "WixCloseApplications is scheduled once");
    assert_eq!(attr(moved[0], "Before"), Some("InstallValidate"));
    assert!(attr(moved[0], "After").is_none() && attr(moved[0], "Sequence").is_none());
    assert_eq!(condition(moved[0]), "", "on every install, upgrade, repair and uninstall");
    assert!(!source.contains("WixCloseApplicationsDeferred"), "no deferred close step");
}

/// "Launch Volatus" on the last page: unticked, first install or upgrade only, through
/// WixShellExec from the dialog (setup's own window, so not elevated when the user started setup
/// by a double-click).
#[test]
fn launch_volatus_is_offered_unticked_on_the_last_page() {
    let source = source();
    let doc = Document::parse(&source).unwrap();
    assert_eq!(property(&doc, "WIXUI_EXITDIALOGOPTIONALCHECKBOXTEXT"), Some("Launch Volatus"));
    assert!(!all(&doc, "Property").iter().any(|p| attr(p, "Id") == Some("WIXUI_EXITDIALOGOPTIONALCHECKBOX")), "no default: unticked");
    assert_eq!(property(&doc, "WixShellExecTarget"), Some("[#VolatusExe]"));
    let launch = by_id(&doc, "CustomAction", "LaunchVolatus");
    assert_eq!(attr(&launch, "BinaryKey"), Some("WixCA"));
    assert_eq!(attr(&launch, "DllEntry"), Some("WixShellExec"));
    assert_eq!(attr(&launch, "Impersonate"), Some("yes"));
    let published: Vec<Node> = all(&doc, "Publish")
        .into_iter()
        .filter(|p| attr(p, "Event") == Some("DoAction") && attr(p, "Value") == Some("LaunchVolatus"))
        .collect();
    assert_eq!(published.len(), 1);
    assert_eq!((attr(&published[0], "Dialog"), attr(&published[0], "Control")), (Some("ExitDialog"), Some("Finish")));
    assert_eq!(condition(&published[0]), "WIXUI_EXITDIALOGOPTIONALCHECKBOX = 1 AND NOT Installed");
    let end: Vec<Node> = all(&doc, "Publish")
        .into_iter()
        .filter(|p| attr(p, "Dialog") == Some("ExitDialog") && attr(p, "Event") == Some("EndDialog"))
        .collect();
    assert_eq!(end.len(), 1);
    let order = |p: &Node| attr(p, "Order").map_or(0, |o| o.parse::<u32>().unwrap());
    assert!(order(&published[0]) < order(&end[0]), "launch before the dialog ends");
}

/// The feature that holds the component `id`.
fn component_feature<'a>(doc: &'a Document, id: &str) -> Node<'a, 'a> {
    let refs: Vec<Node> = all(doc, "ComponentRef").into_iter().filter(|r| attr(r, "Id") == Some(id)).collect();
    assert_eq!(refs.len(), 1, "{id} is in one feature");
    refs[0].parent().filter(|p| is(p, "Feature")).expect("a Feature")
}
