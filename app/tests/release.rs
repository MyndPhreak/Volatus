//! Static checks on the release flow: release-please's config and manifest, and the release
//! workflow `.github/workflows/release-please.yml` (release-please -> build-msi -> publish-msi).
//! They only read files; nothing here talks to GitHub or runs a workflow. There is no YAML parser
//! in the lock, so the workflows are read line by line: the helpers rely on the files' two-space
//! indentation and treat a job as the lines under a key indented two spaces below `jobs:`.
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The `app` package's version, the one source of the release version.
const VERSION: &str = env!("CARGO_PKG_VERSION");
const WORKFLOW: &str = ".github/workflows/release-please.yml";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("app/ has a parent").to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn json(relative: &str) -> Value {
    serde_json::from_str(&read(relative)).unwrap_or_else(|e| panic!("{relative}: {e}"))
}

fn config() -> Value {
    json("release-please-config.json")
}

/// The config's one package, which must be the repository root `.`.
fn root_package(config: &Value) -> &Map<String, Value> {
    let packages = config.get("packages").and_then(Value::as_object).expect("config has a packages object");
    let paths: Vec<&String> = packages.keys().collect();
    assert_eq!(paths, ["."], "one package, at the repository root, so a commit anywhere counts");
    packages["."].as_object().expect("the package config is an object")
}

fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The text after `key:` on the line indented `depth` spaces, if that line is in `lines`.
fn value<'a>(lines: &[&'a str], depth: usize, key: &str) -> Option<&'a str> {
    lines.iter().find_map(|line| {
        let rest = line.strip_prefix(&" ".repeat(depth))?.strip_prefix(key)?.strip_prefix(':')?;
        (indent(line) == depth).then_some(rest.trim())
    })
}

/// The trimmed lines nested under `key:` (indented `depth` spaces), up to the next line at `depth`
/// or less. Comment and blank lines are left out.
fn children<'a>(lines: &[&'a str], depth: usize, key: &str) -> Vec<&'a str> {
    let header = format!("{}{key}:", " ".repeat(depth));
    let Some(start) = lines.iter().position(|l| l.trim_end() == header) else { return Vec::new() };
    lines[start + 1..]
        .iter()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .take_while(|l| indent(l) > depth)
        .map(|l| l.trim())
        .collect()
}

/// The jobs of a workflow, in file order: (job id, the job's lines).
fn jobs(yaml: &str) -> Vec<(String, Vec<&str>)> {
    let mut jobs: Vec<(String, Vec<&str>)> = Vec::new();
    let mut in_jobs = false;
    for line in yaml.lines() {
        if indent(line) == 0 && !line.trim().is_empty() && !line.starts_with('#') {
            in_jobs = line.trim_end() == "jobs:";
            continue;
        }
        if !in_jobs {
            continue;
        }
        let id = line.strip_prefix("  ").and_then(|l| l.trim_end().strip_suffix(':'));
        match id {
            Some(id) if indent(line) == 2 && !id.starts_with('#') && !id.contains(' ') => jobs.push((id.to_string(), Vec::new())),
            _ => {
                if let Some((_, body)) = jobs.last_mut() {
                    body.push(line);
                }
            }
        }
    }
    jobs
}

fn release_jobs(yaml: &str) -> BTreeMap<String, Vec<&str>> {
    jobs(yaml).into_iter().collect()
}

fn job<'a, 'b>(jobs: &'b BTreeMap<String, Vec<&'a str>>, id: &str) -> &'b [&'a str] {
    jobs.get(id).unwrap_or_else(|| panic!("the release workflow has a job {id}"))
}

/// A job's permissions as `scope: level` lines; `None` when the job states none.
fn permissions(body: &[&str]) -> Option<BTreeSet<String>> {
    match value(body, 4, "permissions")? {
        "" => Some(children(body, 4, "permissions").into_iter().map(str::to_string).collect()),
        "{}" => Some(BTreeSet::new()),
        inline => Some(BTreeSet::from([inline.to_string()])),
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Every `uses:` value in a workflow.
fn uses(yaml: &str) -> Vec<String> {
    yaml.lines()
        .filter_map(|l| l.trim_start().trim_start_matches("- ").strip_prefix("uses:"))
        .map(|v| v.trim().to_string())
        .collect()
}

#[test]
fn the_release_is_a_draft_tagged_vx_y_z_with_minor_bumps_before_1_0() {
    let config = config();
    let package = root_package(&config);

    let get = |key: &str| package.get(key).cloned().unwrap_or(Value::Null);

    assert_eq!(get("release-type"), "simple", "the rust type rewrites the workspace Cargo.toml, which has no [package]");
    assert_eq!(get("include-component-in-tag"), false, "tags are vX.Y.Z");
    assert_eq!(get("draft"), true, "the release stays a draft until its MSI is attached");
    assert_eq!(get("force-tag-creation"), true, "a draft release has no tag otherwise, and build-msi checks out the tag");
    assert_eq!(get("bump-minor-pre-major"), true, "feat! before 1.0.0 bumps the minor version");
    assert_eq!(get("changelog-path"), "CHANGELOG.md");
}

#[test]
fn the_changelog_lists_features_and_fixes_once_each() {
    let config = config();
    let sections = root_package(&config).get("changelog-sections").and_then(Value::as_array).expect("changelog-sections is an array");
    let pairs: Vec<(&str, &str)> = sections
        .iter()
        .map(|s| (s["type"].as_str().expect("a section has a type"), s["section"].as_str().expect("a section has a title")))
        .collect();

    let types: BTreeSet<&str> = pairs.iter().map(|(t, _)| *t).collect();

    assert_eq!(types.len(), pairs.len(), "each commit type has one section: {pairs:?}");
    assert!(pairs.contains(&("feat", "Features")), "{pairs:?}");
    assert!(pairs.contains(&("fix", "Bug Fixes")), "{pairs:?}");
}

#[test]
fn the_version_bump_writes_the_app_package_version() {
    let config = config();
    let extra = root_package(&config).get("extra-files").and_then(Value::as_array).expect("extra-files is an array");
    assert_eq!(extra.len(), 1, "one extra file: {extra:?}");
    assert_eq!(extra[0]["type"], "toml");
    assert_eq!(extra[0]["path"], "app/Cargo.toml", "relative to the package path `.`");
    let jsonpath = extra[0]["jsonpath"].as_str().expect("the entry has a jsonpath");
    let segments: Vec<&str> = jsonpath.strip_prefix("$.").expect("the jsonpath starts at $").split('.').collect();
    assert!(
        segments.iter().all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')),
        "a plain dotted path, which reads the same here and in release-please: {jsonpath}"
    );

    let manifest: toml::Table = read("app/Cargo.toml").parse().expect("app/Cargo.toml is TOML");
    let mut node = toml::Value::Table(manifest);
    for segment in &segments {
        node = node.get(segment).unwrap_or_else(|| panic!("{jsonpath} names nothing in app/Cargo.toml")).clone();
    }

    // release-please only warns when a jsonpath matches nothing, and the release would keep the
    // old version; the tag check in build-msi would then fail the release.
    assert_eq!(node.as_str(), Some(VERSION), "{jsonpath} in app/Cargo.toml is the app package's version");
}

#[test]
fn the_release_seed_agrees_with_app_cargo_toml() {
    let config = config();
    let package = root_package(&config);
    let manifest = json(".release-please-manifest.json");
    let manifest = manifest.as_object().expect("the manifest is an object");
    assert!(manifest.keys().all(|k| k == "."), "the manifest knows only the root package: {manifest:?}");
    // release-please reads bootstrap-sha only at the top level of the config.
    assert!(package.get("bootstrap-sha").is_none(), "bootstrap-sha is set at the top level, not in the package");

    match manifest.get(".") {
        // Before the first release: release-as forces the Cargo.toml version, and only commits
        // after bootstrap-sha go in the first changelog.
        None => {
            assert_eq!(package.get("release-as").and_then(Value::as_str), Some(VERSION), "the first release is app/Cargo.toml's version");
            let sha = config.get("bootstrap-sha").and_then(Value::as_str).unwrap_or_default();
            assert!(is_full_sha(sha), "bootstrap-sha is a full 40-character commit SHA (release-please compares full SHAs): {sha:?}");
        }
        // After a release: the release pull request bumps the manifest and Cargo.toml together.
        Some(released) => assert_eq!(released.as_str(), Some(VERSION), "the manifest and app/Cargo.toml carry the same version"),
    }
}

#[test]
fn the_jobs_run_release_please_then_build_then_publish() {
    let yaml = read(WORKFLOW);
    let jobs = jobs(&yaml);

    let graph: Vec<(&str, Option<&str>)> = jobs.iter().map(|(id, body)| (id.as_str(), value(body, 4, "needs"))).collect();

    assert_eq!(
        graph,
        [("release-please", None), ("build-msi", Some("release-please")), ("publish-msi", Some("[release-please, build-msi]"))]
    );
}

#[test]
fn the_build_runs_only_when_release_please_created_a_release() {
    let yaml = read(WORKFLOW);
    let jobs = release_jobs(&yaml);

    let outputs = children(job(&jobs, "release-please"), 4, "outputs");
    let condition = value(job(&jobs, "build-msi"), 4, "if");

    assert!(outputs.contains(&"release_created: ${{ steps.release.outputs.release_created }}"), "{outputs:?}");
    assert!(outputs.contains(&"tag_name: ${{ steps.release.outputs.tag_name }}"), "{outputs:?}");
    assert_eq!(condition, Some("needs.release-please.outputs.release_created == 'true'"));
}

#[test]
fn each_job_has_only_the_permissions_it_needs() {
    let yaml = read(WORKFLOW);
    let lines: Vec<&str> = yaml.lines().collect();
    let jobs = release_jobs(&yaml);

    let stated: BTreeMap<&str, Option<BTreeSet<String>>> = jobs.iter().map(|(id, body)| (id.as_str(), permissions(body))).collect();

    assert_eq!(value(&lines, 0, "permissions"), Some("{}"), "the workflow grants nothing by default");
    assert_eq!(
        stated,
        BTreeMap::from([
            ("release-please", Some(set(&["contents: write", "pull-requests: write"]))),
            ("build-msi", Some(set(&["contents: read"]))),
            ("publish-msi", Some(set(&["contents: write"]))),
        ])
    );
}

#[test]
fn every_action_is_pinned_to_one_full_commit_sha() {
    let dir = repo().join(".github/workflows");
    let mut pins: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.expect("a directory entry").path();
        let yaml = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for used in uses(&yaml) {
            // owner/repo@<40 hex> # <tag or branch>
            let (action, rest) = used.split_once('@').unwrap_or_else(|| panic!("{}: {used} has no @", path.display()));
            let (sha, comment) = rest.split_once(" # ").unwrap_or_else(|| panic!("{}: {used} has no # version comment", path.display()));
            assert!(is_full_sha(sha), "{}: {used} is pinned to a full commit SHA", path.display());
            assert!(!comment.trim().is_empty(), "{}: {used} names its version", path.display());
            pins.entry(action.to_string()).or_default().insert(sha.to_string());
        }
    }
    let release: BTreeSet<String> = uses(&read(WORKFLOW)).iter().map(|u| u.split('@').next().unwrap_or_default().to_string()).collect();

    assert!(pins.values().all(|shas| shas.len() == 1), "an action has the same pin in every workflow: {pins:?}");
    assert_eq!(
        release,
        set(&["actions/checkout", "actions/download-artifact", "actions/upload-artifact", "dtolnay/rust-toolchain", "googleapis/release-please-action"])
    );
}

#[test]
fn the_build_checks_out_the_tag_without_credentials_and_runs_the_shared_script() {
    let yaml = read(WORKFLOW);
    let jobs = release_jobs(&yaml);
    let body: Vec<&str> = job(&jobs, "build-msi").iter().map(|l| l.trim()).collect();

    for expected in [
        "ref: ${{ needs.release-please.outputs.tag_name }}",
        "persist-credentials: false",
        "./.github/scripts/build-msi.ps1 -Version $version",
        "if-no-files-found: error",
    ] {
        assert!(body.contains(&expected), "build-msi has `{expected}`");
    }
}

#[test]
fn the_publish_job_attaches_the_msi_before_it_publishes_the_draft() {
    let yaml = read(WORKFLOW);
    let jobs = release_jobs(&yaml);
    let body = job(&jobs, "publish-msi");
    let line_of = |needle: &str| body.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("publish-msi runs `{needle}`"));

    let upload = line_of("gh release upload \"$TAG\" \"$expected\" --repo \"$GITHUB_REPOSITORY\" --clobber");
    let publish = line_of("gh release edit \"$TAG\" --repo \"$GITHUB_REPOSITORY\" --draft=false");

    assert!(upload < publish, "the draft is published only after its MSI is attached");
}

#[test]
fn the_workflow_runs_for_pushes_to_main_one_run_at_a_time() {
    let yaml = read(WORKFLOW);
    let lines: Vec<&str> = yaml.lines().collect();

    let triggers = children(&lines, 0, "on");
    let concurrency = children(&lines, 0, "concurrency");

    assert_eq!(triggers, ["push:", "branches: [main]"]);
    assert!(concurrency.contains(&"cancel-in-progress: false"), "a running release is never cancelled: {concurrency:?}");
}
