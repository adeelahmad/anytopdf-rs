//! Where the face index lives by default.

use std::{env, path::PathBuf};

/// The per-user anytopdf data directory: `ANYTOPDF_DATA_DIR`, else
/// `$XDG_DATA_HOME/anytopdf` or `~/.local/share/anytopdf` on Linux,
/// `~/Library/Application Support/anytopdf` on macOS and
/// `%APPDATA%\anytopdf` on Windows.
pub fn data_dir() -> Option<PathBuf> {
    let var = |name: &str| {
        env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = var("ANYTOPDF_DATA_DIR") {
        return Some(dir);
    }
    if cfg!(windows) {
        return var("APPDATA").map(|d| d.join("anytopdf"));
    }
    let home = var("HOME");
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/anytopdf"));
    }
    var("XDG_DATA_HOME")
        .or_else(|| home.map(|h| h.join(".local/share")))
        .map(|d| d.join("anytopdf"))
}

/// The face index: `ANYTOPDF_FACE_INDEX`, else `faces.sqlite` in [`data_dir`].
pub fn default_index() -> Option<PathBuf> {
    env::var_os("ANYTOPDF_FACE_INDEX")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| data_dir().map(|d| d.join("faces.sqlite")))
}
