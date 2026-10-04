#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitClass {
    Success,
    Internal,
    Usage,
    Input,
    Provider,
    Strict,
    Render,
    FailFast,
}

impl ExitClass {
    pub const ALL: [ExitClass; 8] = [
        ExitClass::Success,
        ExitClass::Internal,
        ExitClass::Usage,
        ExitClass::Input,
        ExitClass::Provider,
        ExitClass::Strict,
        ExitClass::Render,
        ExitClass::FailFast,
    ];

    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn name(self) -> &'static str {
        match self {
            ExitClass::Success => "success",
            ExitClass::Internal => "internal",
            ExitClass::Usage => "usage",
            ExitClass::Input => "input",
            ExitClass::Provider => "provider",
            ExitClass::Strict => "strict",
            ExitClass::Render => "render",
            ExitClass::FailFast => "fail-fast",
        }
    }
}

#[derive(Debug)]
pub struct CliError {
    pub class: ExitClass,
    pub error: anyhow::Error,
}

impl<E: Into<anyhow::Error>> From<E> for CliError {
    fn from(error: E) -> Self {
        CliError {
            class: ExitClass::Internal,
            error: error.into(),
        }
    }
}

/// Tag a fallible result with the exit class its failure maps to.
pub fn tag<T, E: Into<anyhow::Error>>(
    class: ExitClass,
    result: Result<T, E>,
) -> Result<T, CliError> {
    result.map_err(|e| CliError {
        class,
        error: e.into(),
    })
}

pub fn fail(class: ExitClass, message: impl std::fmt::Display) -> CliError {
    CliError {
        class,
        error: anyhow::anyhow!("{message}"),
    }
}

/// Strips local directory prefixes from user-visible text (share profile only).
#[derive(Default)]
pub struct Redactor {
    enabled: bool,
    dirs: Vec<String>,
}

impl Redactor {
    pub fn new(enabled: bool) -> Self {
        Redactor {
            enabled,
            dirs: Vec::new(),
        }
    }

    /// Register the directory containing `path`, as typed, absolute and canonical.
    pub fn add_parent_of(&mut self, path: &std::path::Path) {
        if !self.enabled {
            return;
        }
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        for candidate in [Some(absolute.clone()), absolute.canonicalize().ok()]
            .into_iter()
            .flatten()
        {
            if let Some(parent) = candidate.parent() {
                self.add_dir(parent);
            }
        }
    }

    pub fn add_dir(&mut self, dir: &std::path::Path) {
        if !self.enabled || dir.parent().is_none() {
            return;
        }
        let text = dir.to_string_lossy().into_owned();
        let mut forms = vec![text.clone()];
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            forms.push(stripped.to_string());
        }
        for form in forms {
            if !self.dirs.contains(&form) {
                self.dirs.push(form);
            }
        }
        self.dirs.sort_by_key(|d| std::cmp::Reverse(d.len()));
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for dir in &self.dirs {
            for sep in ['/', '\\'] {
                out = out.replace(&format!("{dir}{sep}"), "");
            }
            out = out.replace(dir.as_str(), ".");
        }
        out
    }

    pub fn path(&self, path: &std::path::Path) -> String {
        self.apply(&path.display().to_string())
    }
}

/// Print the batch summary and per-input skip reasons to stderr.
pub fn print_summary(
    converted: usize,
    skipped: &[&anytopdf_core::Diagnostic],
    redactor: &Redactor,
) {
    eprintln!("Summary: {converted} converted, {} skipped", skipped.len());
    for d in skipped {
        let input = d
            .input
            .as_ref()
            .map_or_else(String::new, |p| redactor.path(p));
        eprintln!(
            "  skipped {input}: [{}] {}",
            d.code.as_str(),
            redactor.apply(&d.message)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn exit_codes_are_distinct_and_documented_in_readme() {
        let readme = include_str!("../../../README.md");
        let expected = [
            "success",
            "internal",
            "usage",
            "input",
            "provider",
            "strict",
            "render",
            "fail-fast",
        ];
        let codes: BTreeSet<u8> = ExitClass::ALL.iter().map(|c| c.code()).collect();
        assert_eq!(codes, (0..=7).collect::<BTreeSet<u8>>());
        assert_eq!(ExitClass::ALL.len(), 8);
        for (index, class) in ExitClass::ALL.iter().enumerate() {
            assert_eq!(class.code() as usize, index, "{class:?}");
            assert_eq!(class.name(), expected[index]);
            let row = format!("| {} | {} |", class.code(), class.name());
            assert!(
                readme.lines().any(|line| line.starts_with(&row)),
                "README lacks row {row}"
            );
        }
    }
}
