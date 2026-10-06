//! Per-user locations shared by the CLI and the plugins it ships with.

use std::{
    env,
    path::{Path, PathBuf},
};

/// Overrides [`user_data_dir`]; also how tests and portable installs relocate it.
pub const DATA_DIR_ENV: &str = "ANYTOPDF_DATA_DIR";

/// Where anytopdf keeps downloaded models and setup records: `ANYTOPDF_DATA_DIR`
/// when set, otherwise the platform's per-user data folder plus `anytopdf`
/// (`~/Library/Application Support` on macOS, `%LOCALAPPDATA%` on Windows,
/// `$XDG_DATA_HOME` or `~/.local/share` elsewhere). `None` when no home folder
/// can be found.
pub fn user_data_dir() -> Option<PathBuf> {
    data_dir_from(|name| env::var_os(name).filter(|v| !v.is_empty()))
}

fn data_dir_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(dir) = var(DATA_DIR_ENV) {
        return Some(PathBuf::from(dir));
    }
    let base = if cfg!(windows) {
        var("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| var("USERPROFILE").map(|home| Path::new(&home).join("AppData/Local")))?
    } else if cfg!(target_os = "macos") {
        PathBuf::from(var("HOME")?).join("Library/Application Support")
    } else {
        var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| var("HOME").map(|home| Path::new(&home).join(".local/share")))?
    };
    Some(base.join("anytopdf"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, ffi::OsString};

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn explicit_data_dir_wins_over_platform_default() {
        let dir = data_dir_from(vars(&[("ANYTOPDF_DATA_DIR", "/srv/a"), ("HOME", "/h")]));
        assert_eq!(dir, Some(PathBuf::from("/srv/a")));
    }

    #[test]
    fn platform_default_is_under_the_user_home() {
        let dir = data_dir_from(vars(&[
            ("HOME", "/home/u"),
            ("LOCALAPPDATA", "C:/Users/u/AppData/Local"),
        ]))
        .unwrap();
        assert!(dir.ends_with("anytopdf"), "{}", dir.display());
        if cfg!(windows) {
            assert!(dir.starts_with("C:/Users/u/AppData/Local"));
        } else if cfg!(target_os = "macos") {
            assert_eq!(
                dir,
                Path::new("/home/u/Library/Application Support/anytopdf")
            );
        } else {
            assert_eq!(dir, Path::new("/home/u/.local/share/anytopdf"));
            let xdg = data_dir_from(vars(&[("XDG_DATA_HOME", "/x"), ("HOME", "/home/u")]));
            assert_eq!(xdg, Some(PathBuf::from("/x/anytopdf")));
        }
        assert_eq!(data_dir_from(vars(&[])), None);
    }
}
