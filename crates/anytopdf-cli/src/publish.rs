use crate::naming;
use anyhow::{Context, Result, bail};
use anytopdf_core::atomic_write;
use std::path::{Path, PathBuf};

pub(crate) fn checked_destination(
    path: &Path,
    sources: &[PathBuf],
    overwrite: bool,
) -> Result<PathBuf> {
    let absolute = naming::resolve(path)?;
    if sources.contains(&absolute) {
        bail!("output would overwrite an input: {}", path.display());
    }
    if path.is_dir() {
        bail!("output is a directory: {}", path.display());
    }
    if path.exists() && !overwrite {
        bail!(
            "output exists: {}; use --overwrite to replace it",
            path.display()
        );
    }
    Ok(absolute)
}

pub(crate) fn publish_output(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    if overwrite {
        return atomic_write(path, bytes);
    }
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(path)
        .with_context(|| format!("publish {}", path.display()))?;
    Ok(())
}
