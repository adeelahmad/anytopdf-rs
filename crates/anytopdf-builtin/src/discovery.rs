use anyhow::{Context, Result};
use regex::Regex;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Debug, Default, Clone)]
pub struct DiscoveryOptions {
    pub include_hidden: bool,
    pub filter: Option<Regex>,
}

pub fn discover_inputs(inputs: &[PathBuf], opts: &DiscoveryOptions) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for input in inputs {
        if input.is_file() {
            push_if_match(input, opts, &mut out, &mut seen)?;
            continue;
        }

        if input.is_dir() {
            for entry in WalkDir::new(input)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| {
                    opts.include_hidden || entry.depth() == 0 || !is_hidden(entry.path(), input)
                })
            {
                let entry = entry.with_context(|| format!("walk {}", input.display()))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                if !opts.include_hidden && is_hidden(entry.path(), input) {
                    continue;
                }
                push_if_match(entry.path(), opts, &mut out, &mut seen)?;
            }
            continue;
        }

        anyhow::bail!("input not found: {}", input.display());
    }

    out.sort();
    Ok(out)
}

fn is_hidden(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .into_iter()
        .flat_map(|p| p.components())
        .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
}

fn push_if_match(
    path: &Path,
    opts: &DiscoveryOptions,
    out: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))?;
    if let Some(filter) = &opts.filter
        && !filter.is_match(&canonical.to_string_lossy())
    {
        return Ok(());
    }
    if seen.insert(canonical.clone()) {
        out.push(canonical);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovery_prunes_hidden_directories_sorts_and_deduplicates() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".hidden")).unwrap();
        fs::write(root.path().join(".hidden/private.txt"), "secret").unwrap();
        fs::write(root.path().join("b.txt"), "B").unwrap();
        fs::write(root.path().join("a.txt"), "A").unwrap();
        let inputs = vec![root.path().into(), root.path().join("a.txt")];
        let paths = discover_inputs(&inputs, &DiscoveryOptions::default()).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("a.txt"));
        let all = discover_inputs(
            &inputs,
            &DiscoveryOptions {
                include_hidden: true,
                filter: None,
            },
        )
        .unwrap();
        assert_eq!(all.len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn directory_scans_skip_symlinked_files_and_directories() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "outside").unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("real.txt"), "inside").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            root.path().join("link.txt"),
        )
        .unwrap();
        symlink(outside.path(), root.path().join("linked-dir")).unwrap();
        symlink(root.path(), root.path().join("loop")).unwrap();
        let options = DiscoveryOptions {
            include_hidden: true,
            filter: None,
        };
        let paths = discover_inputs(&[root.path().into()], &options).unwrap();
        assert_eq!(
            paths,
            vec![root.path().canonicalize().unwrap().join("real.txt")]
        );
        let named = discover_inputs(&[root.path().join("link.txt")], &options).unwrap();
        assert_eq!(
            named,
            vec![outside.path().canonicalize().unwrap().join("secret.txt")],
            "a symlink named on the command line is followed"
        );
    }

    #[test]
    fn hidden_directory_named_explicitly_is_scanned() {
        let root = tempfile::tempdir().unwrap();
        let hidden = root.path().join(".notes");
        fs::create_dir(&hidden).unwrap();
        fs::write(hidden.join("a.txt"), "A").unwrap();
        fs::write(hidden.join(".b.txt"), "B").unwrap();
        let paths =
            discover_inputs(std::slice::from_ref(&hidden), &DiscoveryOptions::default()).unwrap();
        assert_eq!(paths, vec![hidden.canonicalize().unwrap().join("a.txt")]);
    }

    #[test]
    fn missing_input_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            discover_inputs(&[root.path().join("missing")], &DiscoveryOptions::default()).is_err()
        );
    }
}
