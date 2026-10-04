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

fn setting<'a>(section: &'a str, key: &str) -> Option<&'a str> {
    section.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.split('#').next().unwrap_or("").trim())
    })
}

fn is_triple(token: &str) -> bool {
    let parts: Vec<&str> = token.split('-').collect();
    parts.len() >= 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        && parts[0]
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn integer_after(line: &str, word: &str) -> bool {
    let lower = line.to_lowercase();
    lower.find(word).is_some_and(|i| {
        lower[i + word.len()..]
            .split(|c: char| !c.is_ascii_digit())
            .any(|run| !run.is_empty())
    })
}

#[test]
fn release_profile_enables_lto_and_strip() {
    let manifest = repo_file("Cargo.toml");
    let start = manifest
        .find("[profile.release]")
        .expect("root Cargo.toml has a [profile.release] section");
    let body = &manifest[start + "[profile.release]".len()..];
    let section = body
        .split_once('\n')
        .map_or("", |(_, rest)| rest)
        .lines()
        .take_while(|l| !l.starts_with('['))
        .collect::<Vec<_>>()
        .join("\n");
    let lto = setting(&section, "lto");
    assert!(
        matches!(lto, Some("true" | "\"fat\"" | "\"thin\"")),
        "lto must be true, \"fat\" or \"thin\", got {lto:?}"
    );
    let strip = setting(&section, "strip");
    assert!(
        matches!(strip, Some("true" | "\"symbols\"")),
        "strip must be true or \"symbols\", got {strip:?}"
    );
}

#[test]
fn releasing_records_binary_sizes_before_and_after() {
    let doc = repo_file("RELEASING.md");
    let lines: Vec<&str> = doc.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with('#') && l.contains("Release binary size"))
        .expect("RELEASING.md has a `Release binary size` heading");
    let section: Vec<&str> = lines[start + 1..]
        .iter()
        .copied()
        .take_while(|l| !l.starts_with('#'))
        .collect();
    for word in ["before", "after"] {
        assert!(
            section.iter().any(|l| integer_after(l, word)),
            "section lacks `{word}` followed by an integer byte count"
        );
    }
    assert!(
        section
            .iter()
            .flat_map(|l| l.split(|c: char| c.is_whitespace() || "`,;:()".contains(c)))
            .any(is_triple),
        "section lacks a target triple"
    );
}
