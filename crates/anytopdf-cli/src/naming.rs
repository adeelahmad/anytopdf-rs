use std::path::{Path, PathBuf};

pub fn next_free_path(base: &Path, protected: &[PathBuf]) -> anyhow::Result<PathBuf> {
    let taken = |p: &Path| p.exists() || protected.iter().any(|q| q == p);
    if !taken(base) {
        return Ok(base.to_path_buf());
    }
    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    let ext = base
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 1..=u32::MAX {
        let candidate = base.with_file_name(format!("{stem}-{n}{ext}"));
        if !taken(&candidate) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("no free output name for {}", base.display())
}

pub fn default_output(inputs: &[PathBuf], cwd: &Path) -> PathBuf {
    match inputs {
        [one] => match one.file_stem() {
            Some(stem) => cwd.join(format!("{}.pdf", stem.to_string_lossy())),
            None => cwd.join("anytopdf.pdf"),
        },
        _ => cwd.join("anytopdf.pdf"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn next_free_path_skips_existing_and_protected_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        fs::write(dir.join("notes.pdf"), "x").unwrap();
        fs::write(dir.join("notes-1.pdf"), "x").unwrap();
        let protected = vec![dir.join("notes-2.pdf")];
        assert_eq!(
            next_free_path(&dir.join("notes.pdf"), &protected).unwrap(),
            dir.join("notes-3.pdf")
        );
        assert_eq!(
            next_free_path(&dir.join("fresh.pdf"), &protected).unwrap(),
            dir.join("fresh.pdf")
        );
        fs::write(dir.join("bare"), "x").unwrap();
        assert_eq!(
            next_free_path(&dir.join("bare"), &[]).unwrap(),
            dir.join("bare-1")
        );
    }

    #[test]
    fn default_output_uses_stem_for_one_input_and_anytopdf_for_several() {
        let cwd = PathBuf::from("/work/cwd");
        let one = [PathBuf::from("/in/notes.txt")];
        assert_eq!(default_output(&one, &cwd), cwd.join("notes.pdf"));
        let two = [PathBuf::from("/in/a.txt"), PathBuf::from("/in/b.txt")];
        assert_eq!(default_output(&two, &cwd), cwd.join("anytopdf.pdf"));
    }
}
