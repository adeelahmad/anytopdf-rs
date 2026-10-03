use anyhow::{Context, Result};
use std::{io::Write, path::Path};

/// Stage beside the destination so a failed write never truncates an existing file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_parents_and_replaces_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/result.json");
        atomic_write(&path, b"original").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }
}
