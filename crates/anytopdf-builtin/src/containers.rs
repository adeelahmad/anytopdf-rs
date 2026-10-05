//! Shared plumbing for importers whose inputs contain other files (email
//! attachments, archive members): writing members into the job workspace under
//! safe names and importing them through the pipeline's [`MemberImporter`].

use anyhow::{Context, Result};
use anytopdf_core::*;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Longest file name (in characters) a member keeps; longer names are cut
/// before the extension.
const MAX_NAME_CHARS: usize = 96;

/// A fresh directory in the job workspace for one container's members.
pub(crate) fn member_dir(ctx: &JobContext, kind: &str) -> Result<PathBuf> {
    let dir = ctx.workspace.join(format!("{kind}-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Reduces an untrusted member name to one safe path component. Directory
/// parts, `..`, drive prefixes, control and reserved characters are removed,
/// so the result can never escape the directory it is joined to.
pub(crate) fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if cleaned.is_empty() {
        return "member".into();
    }
    if cleaned.chars().count() <= MAX_NAME_CHARS {
        return cleaned.into();
    }
    let (stem, ext) = match cleaned.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.chars().count() <= 10 => (stem, ext),
        _ => (cleaned, ""),
    };
    let keep = MAX_NAME_CHARS - ext.chars().count() - 1;
    let stem: String = stem.chars().take(keep).collect();
    if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    }
}

/// `<dir>/<index>-<safe name>`; the index keeps members with the same name apart.
pub(crate) fn member_path(dir: &Path, index: usize, name: &str) -> PathBuf {
    dir.join(format!("{index:04}-{}", safe_file_name(name)))
}

/// Writes one member at [`member_path`].
pub(crate) fn write_member(dir: &Path, index: usize, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let path = member_path(dir, index, name);
    fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Why [`copy_capped`] stopped before the end of its input.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CopyLimit {
    /// More than `cap` bytes were available.
    TooLarge,
    /// The output outgrew `max_ratio` times the compressed size.
    Ratio,
}

/// Copies at most `cap` bytes, failing as soon as the input proves longer, or
/// as soon as the output exceeds `max_ratio` times `compressed` bytes (once
/// past `ratio_floor`). Nothing is trusted from archive headers: the limits
/// are checked against bytes actually produced.
pub(crate) fn copy_capped(
    mut reader: impl Read,
    mut writer: impl Write,
    cap: u64,
    compressed: Option<u64>,
    max_ratio: u64,
    ratio_floor: u64,
) -> Result<std::result::Result<u64, CopyLimit>> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(Ok(written)),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("read member"),
        };
        written += n as u64;
        if written > cap {
            return Ok(Err(CopyLimit::TooLarge));
        }
        if let Some(c) = compressed
            && written > ratio_floor
            && written > c.max(1).saturating_mul(max_ratio)
        {
            return Ok(Err(CopyLimit::Ratio));
        }
        writer.write_all(&buf[..n]).context("write member")?;
    }
}

/// A member file written into the workspace, with the name shown to readers.
pub(crate) struct MemberFile {
    pub label: String,
    pub path: PathBuf,
}

/// Imports members and re-parents their units onto `container`. A member that
/// cannot be imported becomes an `input.members-not-imported` warning rather
/// than failing the container.
pub(crate) fn import_members(
    ctx: &JobContext,
    members: &dyn MemberImporter,
    container: &SourceRecord,
    files: Vec<MemberFile>,
) -> (Vec<Unit>, Vec<String>) {
    let container_name = anytopdf_core::basename(&container.path);
    let mut units = Vec::new();
    let mut warnings = Vec::new();
    for file in files {
        match members.import_member(ctx, &file.path) {
            Ok(outcome) => {
                warnings.extend(outcome.warnings);
                for mut unit in outcome.units {
                    unit.source_id = container.id;
                    unit.metadata
                        .entry("container.member".into())
                        .or_insert_with(|| file.label.clone());
                    units.push(unit);
                }
            }
            Err(e) => warnings.push(
                Diagnostic::new(
                    DiagnosticCode::MembersNotImported,
                    format!("{container_name}: {} was not imported: {e:#}", file.label),
                )
                .to_string(),
            ),
        }
    }
    (units, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_file_name_cannot_escape_its_directory() {
        for (raw, want) in [
            ("../../etc/passwd", "passwd"),
            ("..\\..\\boot.ini", "boot.ini"),
            ("C:\\Users\\x\\a.txt", "a.txt"),
            ("/abs/path/b.pdf", "b.pdf"),
            ("..", "member"),
            ("", "member"),
            ("dir/", "member"),
            ("a\u{0}b?.png", "a_b_.png"),
            ("  .hidden  ", "hidden"),
        ] {
            assert_eq!(safe_file_name(raw), want, "{raw:?}");
        }
        let long = format!("{}.pdf", "x".repeat(500));
        assert_eq!(
            member_path(Path::new("d"), 7, "../x.txt"),
            Path::new("d").join("0007-x.txt")
        );
        let short = safe_file_name(&long);
        assert_eq!(short.chars().count(), MAX_NAME_CHARS);
        assert!(short.ends_with(".pdf"), "{short}");
    }

    #[test]
    fn copy_capped_stops_on_size_and_ratio_using_real_bytes() {
        let data = vec![0u8; 300 * 1024];
        let mut out = Vec::new();
        let r = copy_capped(&data[..], &mut out, 1 << 20, None, 100, 0).unwrap();
        assert_eq!(r, Ok(data.len() as u64));
        assert_eq!(out.len(), data.len());
        let r = copy_capped(&data[..], std::io::sink(), 100 * 1024, None, 100, 0).unwrap();
        assert_eq!(r, Err(CopyLimit::TooLarge));
        let r = copy_capped(&data[..], std::io::sink(), 1 << 20, Some(1024), 100, 0).unwrap();
        assert_eq!(r, Err(CopyLimit::Ratio));
        let r = copy_capped(
            &data[..],
            std::io::sink(),
            1 << 20,
            Some(1024),
            100,
            1 << 20,
        )
        .unwrap();
        assert_eq!(
            r,
            Ok(data.len() as u64),
            "below the floor the ratio is not checked"
        );
    }
}
