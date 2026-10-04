use std::fs;
use std::path::PathBuf;

fn repo_file(name: &str) -> String {
    let root: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join("crates").is_dir())
        .expect("workspace root")
        .to_path_buf();
    fs::read_to_string(root.join(name)).unwrap_or_else(|err| panic!("read {name}: {err}"))
}

fn missing<'a>(text: &str, needles: &[&'a str]) -> Vec<&'a str> {
    let lower = text.to_lowercase();
    needles
        .iter()
        .copied()
        .filter(|n| !lower.contains(&n.to_lowercase()))
        .collect()
}

fn semver(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>());
    let v = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    parts.next().is_none().then_some(v)
}

#[test]
fn changelog_documents_every_sprint_behaviour_change() {
    let changelog = repo_file("CHANGELOG.md");
    let section = changelog
        .split("\n## ")
        .nth(1)
        .expect("CHANGELOG.md has a `## ` section");
    let (heading, _) = section.split_once('\n').unwrap_or((section, ""));
    let version = semver(heading).unwrap_or_else(|| panic!("heading is not SemVer: {heading:?}"));
    assert!(
        version > (0, 1, 0),
        "first CHANGELOG section must be newer than 0.1.0, got {heading:?}"
    );
    let absent = missing(
        section,
        &[
            "text layer",
            "provenance page",
            "--no-provenance-page",
            "--profile",
            "SHA-256",
            "anchor",
            "SOURCE_DATE_EPOCH",
            "graph dump",
            "diagnostic",
            "--strict",
            "exit code",
            "--fail-fast",
            "-o",
            "--json",
            "capabilities",
            "extract",
            "manifest",
            "lossy",
            "frames",
            "--transcript",
            "--output-dir",
            "help",
            "LTO",
        ],
    );
    assert!(absent.is_empty(), "CHANGELOG section missing: {absent:?}");
}

#[test]
fn architecture_describes_new_layers() {
    let absent = missing(
        &repo_file("ARCHITECTURE.md"),
        &[
            "Diagnostic",
            "content-derived",
            "anchor",
            "profile",
            "manifest",
            "attachment",
            "extract",
            "exit code",
        ],
    );
    assert!(absent.is_empty(), "ARCHITECTURE.md missing: {absent:?}");
}

#[test]
fn plugin_protocol_documents_additive_fields() {
    let protocol = repo_file("PLUGIN_PROTOCOL.md");
    let absent = missing(
        &protocol,
        &["plugin.warning", "sha256", "anchor", "unit_pages"],
    );
    assert!(absent.is_empty(), "PLUGIN_PROTOCOL.md missing: {absent:?}");
    assert!(
        protocol.contains("\"protocol\": 1"),
        "PLUGIN_PROTOCOL.md must still state protocol 1"
    );
}

#[test]
fn schemas_readme_lists_the_events_schema() {
    let readme = repo_file("schemas/README.md");
    let absent = missing(
        &readme,
        &[
            "exactly 9 files",
            "events",
            "anytopdf.events/1",
            "--events",
            "stderr",
        ],
    );
    assert!(absent.is_empty(), "schemas/README.md is missing {absent:?}");
}
