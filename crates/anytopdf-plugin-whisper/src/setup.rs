//! whisper.cpp model catalog, checksummed installs and the setup record that
//! lets the plugin find a model without `ANYTOPDF_WHISPER_MODEL`.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// One ggml model published by whisper.cpp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    pub name: &'static str,
    /// Approximate download size, for the progress line.
    pub size_mib: u32,
    /// SHA-1 of `ggml-<name>.bin`, as listed in whisper.cpp's `models/README.md`.
    pub sha1: &'static str,
}

/// Models `anytopdf setup whisper --model` accepts. Names ending in `.en` are
/// English-only; `-q5_0` models are quantized.
pub const MODELS: &[Model] = &[
    model("tiny", 75, "bd577a113a864445d4c299885e0cb97d4ba92b5f"),
    model("tiny.en", 75, "c78c86eb1a8faa21b369bcd33207cc90d64ae9df"),
    model("base", 142, "465707469ff3a37a2b9b8d8f89f2f99de7299dac"),
    model("base.en", 142, "137c40403d78fd54d454da0f9bd998f78703390c"),
    model("small", 466, "55356645c2b361a969dfd0ef2c5a50d530afd8d5"),
    model("small.en", 466, "db8a495a91d927739e50b3fc1cc4c6b8f6c2d022"),
    model("medium", 1536, "fd9727b6e1217c2f614f9b698455c4ffd82463b4"),
    model(
        "medium.en",
        1536,
        "8c30f0e44ce9560643ebd10bbe50cd20eafd3723",
    ),
    model("large-v3", 2970, "ad82bf6a9043ceed055076d0fd39f5f186ff8062"),
    model(
        "large-v3-q5_0",
        1126,
        "e6e2ed78495d403bef4b7cff42ef4aaadcfea8de",
    ),
    model(
        "large-v3-turbo",
        1536,
        "4af2b29d7ec73d781377bfd1758ca957a807e941",
    ),
    model(
        "large-v3-turbo-q5_0",
        547,
        "e050f7970618a659205450ad97eb95a18d69c9ee",
    ),
];

const fn model(name: &'static str, size_mib: u32, sha1: &'static str) -> Model {
    Model {
        name,
        size_mib,
        sha1,
    }
}

/// Multilingual and small enough for a laptop; language is detected per file.
pub const DEFAULT_MODEL: &str = "base";
/// Where whisper.cpp publishes its ggml models.
pub const DEFAULT_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
/// `schema_version` of [`Record`].
pub const RECORD_SCHEMA: &str = "anytopdf.whisper-setup/1";

pub fn find_model(name: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.name == name)
}

pub fn file_name(model: &Model) -> String {
    format!("ggml-{}.bin", model.name)
}

pub fn download_url(base: &str, model: &Model) -> String {
    format!("{}/{}", base.trim_end_matches('/'), file_name(model))
}

/// `<data dir>/whisper`, where models and the setup record live.
pub fn whisper_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("whisper")
}

fn record_path(data_dir: &Path) -> PathBuf {
    whisper_dir(data_dir).join("setup.json")
}

/// What `anytopdf setup whisper` installed, kept in `<data dir>/whisper/setup.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema_version: String,
    pub model: String,
    pub path: PathBuf,
    pub sha256: String,
}

pub fn read_record(data_dir: &Path) -> Result<Option<Record>> {
    let path = record_path(data_dir);
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let record: Record =
        serde_json::from_slice(&raw).with_context(|| format!("parse {}", path.display()))?;
    if record.schema_version != RECORD_SCHEMA {
        bail!(
            "{} has schema {:?}, expected {RECORD_SCHEMA:?}",
            path.display(),
            record.schema_version
        );
    }
    Ok(Some(record))
}

pub fn write_record(data_dir: &Path, record: &Record) -> Result<()> {
    let path = record_path(data_dir);
    let temporary = path.with_extension("json.tmp");
    fs::create_dir_all(whisper_dir(data_dir))
        .with_context(|| format!("create {}", whisper_dir(data_dir).display()))?;
    fs::write(&temporary, serde_json::to_vec_pretty(record)?)
        .with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, &path).with_context(|| format!("write {}", path.display()))
}

/// The model file recorded by `anytopdf setup whisper` in the user data dir,
/// when the record is readable and the file still exists.
pub fn recorded_model() -> Option<PathBuf> {
    let data_dir = anytopdf_core::user_data_dir()?;
    read_record(&data_dir)
        .ok()
        .flatten()
        .map(|record| record.path)
        .filter(|path| path.is_file())
}

/// Hex digests of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digests {
    pub sha1: String,
    pub sha256: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Streams `reader` into `destination`, verifying `expected_sha1` when given.
/// Bytes land in `<destination>.part` first, so an interrupted or mismatched
/// download never leaves a file the plugin would load. `progress` receives the
/// running byte count.
pub fn install(
    mut reader: impl Read,
    destination: &Path,
    expected_sha1: Option<&str>,
    mut progress: impl FnMut(u64),
) -> Result<Digests> {
    let parent = destination
        .parent()
        .context("model destination has no parent folder")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let mut name = destination
        .file_name()
        .context("model destination has no file name")?
        .to_os_string();
    name.push(".part");
    let partial = destination.with_file_name(name);
    let result = (|| {
        let mut file =
            fs::File::create(&partial).with_context(|| format!("create {}", partial.display()))?;
        let (digests, total) = copy_hashing(&mut reader, &mut file, &mut progress)?;
        file.sync_all().ok();
        drop(file);
        if total == 0 {
            bail!("received an empty file");
        }
        if let Some(expected) = expected_sha1
            && !digests.sha1.eq_ignore_ascii_case(expected)
        {
            bail!(
                "checksum mismatch: SHA-1 {} but whisper.cpp publishes {expected}",
                digests.sha1
            );
        }
        fs::rename(&partial, destination)
            .with_context(|| format!("move model into {}", destination.display()))?;
        Ok(digests)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

/// Hashes an installed file without copying it.
pub fn digest_file(path: &Path) -> Result<Digests> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(copy_hashing(&mut file, &mut std::io::sink(), &mut |_| {})?.0)
}

fn copy_hashing(
    reader: &mut impl Read,
    writer: &mut impl Write,
    progress: &mut impl FnMut(u64),
) -> Result<(Digests, u64)> {
    let (mut sha1, mut sha256) = (Sha1::new(), Sha256::new());
    let mut buffer = vec![0; 1 << 16];
    let mut total = 0u64;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("read interrupted"),
        };
        let chunk = &buffer[..read];
        sha1.update(chunk);
        sha256.update(chunk);
        writer.write_all(chunk).context("write model")?;
        total += read as u64;
        progress(total);
    }
    let digests = Digests {
        sha1: hex(&sha1.finalize()),
        sha256: hex(&sha256.finalize()),
    };
    Ok((digests, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_names_are_unique_and_checksums_are_sha1_hex() {
        let mut names: Vec<_> = MODELS.iter().map(|m| m.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), MODELS.len());
        for model in MODELS {
            assert_eq!(model.sha1.len(), 40, "{}", model.name);
            assert!(model.sha1.bytes().all(|b| b.is_ascii_hexdigit()));
        }
        let default = find_model(DEFAULT_MODEL).unwrap();
        assert_eq!(
            download_url(DEFAULT_BASE_URL, default),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin"
        );
        assert_eq!(
            download_url("http://mirror/models/", default),
            "http://mirror/models/ggml-base.bin"
        );
        assert!(find_model("enormous").is_none());
    }

    #[test]
    fn install_verifies_sha1_and_reports_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("whisper/ggml-tiny.bin");
        let mut seen = 0;
        let digests = install(
            &b"abc"[..],
            &destination,
            Some("A9993E364706816ABA3E25717850C26C9CD0D89D"),
            |n| seen = n,
        )
        .unwrap();
        assert_eq!(seen, 3);
        assert_eq!(fs::read(&destination).unwrap(), b"abc");
        assert_eq!(digest_file(&destination).unwrap(), digests);
        assert_eq!(
            digests.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn mismatched_or_empty_downloads_leave_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("ggml-tiny.bin");
        let error =
            install(&b"tampered"[..], &destination, Some(MODELS[0].sha1), |_| {}).unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"), "{error}");
        assert!(install(&b""[..], &destination, None, |_| {}).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn record_round_trips_and_rejects_other_schemas() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_record(dir.path()).unwrap(), None);
        let record = Record {
            schema_version: RECORD_SCHEMA.into(),
            model: "base".into(),
            path: dir.path().join("whisper/ggml-base.bin"),
            sha256: "00".into(),
        };
        write_record(dir.path(), &record).unwrap();
        assert_eq!(read_record(dir.path()).unwrap(), Some(record));
        fs::write(
            dir.path().join("whisper/setup.json"),
            r#"{"schema_version":"other/9","model":"x","path":"/x","sha256":""}"#,
        )
        .unwrap();
        assert!(read_record(dir.path()).is_err());
    }
}
